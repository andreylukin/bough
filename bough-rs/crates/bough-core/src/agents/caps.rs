//! The delegation width caps (port of `src/agents/caps.ts`): how many
//! subagents one turn may launch, and how many may be running at once across
//! the whole tree (spec §7).
//!
//! THE INVARIANT THIS HOLDS: **a refused launch costs nothing.** Not the slot
//! it asked for, not a sibling that already started, not the budget of the
//! turn it was launched from. Fan-out is written as N launches settled
//! together precisely because some of them are expected to be refused, so the
//! cap has to behave like a rejection for exactly one element of that set —
//! every other launch continues, and the ledger afterwards reflects the
//! launches that actually happened and no others.
//!
//! WHY A LEDGER AND NOT A QUERY: counts derived from `db.list_sessions()` are
//! wrong under synchronous fan-out (check-then-create with an await between =
//! twelve checks all see zero). The check and the take are ONE section under
//! ONE mutex ([`SpawnCaps::reserve`]) — the TS version was atomic by
//! construction on a single-threaded runtime; here the Mutex is the port of
//! "synchronous from first read to last write".
//!
//! The concurrency counter is keyed by TREE ([`tree_root_of`]), not session:
//! "four running at once" is a property of a piece of work. The slot is taken
//! at reservation and held until the lease is released; the bus attachment
//! ([`SpawnCaps::attach_bus`]) backstops dropped leases.
//!
//! The *depth* cap is NOT here — it lives in `subagent.rs` with the code that
//! writes lineage. What this module owns of nesting: [`assert_may_delegate`]
//! refuses a *detached* `spawn()` from inside a subagent turn.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use futures::future::BoxFuture;

use crate::bus::Bus;
use crate::errors::{BoughError, ErrorKind};
use crate::schema::events::{BoughEvent, EventType};
use crate::schema::parts::SessionKind;
use crate::types::{Db, TurnCtx};

// ---------------------------------------------------------------------------
// The caps
// ---------------------------------------------------------------------------

/// Total launches — blocking and detached alike — permitted from one turn.
/// Never decremented; waiting does not clear it. Bounds a *sequential* loop,
/// which the concurrency cap cannot.
pub const MAX_SPAWNS_PER_TURN: u32 = 8;

/// Subagent turns permitted in flight at once across one tree (spec §7).
pub const MAX_TREE_CONCURRENT: u32 = 4;

/// How a launch awaits its child, which is the only thing the nesting rule
/// cares about: `Blocking` is `agent()`, `Detached` is `spawn()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelegationMode {
    Blocking,
    Detached,
}

/// The lineage hop limit. Lineage is written once, at spawn, so a cycle can
/// only come from a bad write — this stops such a write from hanging every
/// later launch on an infinite walk.
const MAX_LINEAGE_HOPS: u32 = 16;

// ---------------------------------------------------------------------------
// Tree identity
// ---------------------------------------------------------------------------

/// The top session of a subagent tree: walk `originId` up while the session is
/// a subagent, and stop at the first thing that is not.
///
/// A fork or a compaction is therefore its own tree even though it also
/// carries an `originId` — it is a branch of a conversation, not a delegated
/// child. A dangling origin (or a db hiccup) leaves this session as the top of
/// the tree it can actually see — better a budget scoped slightly too narrowly
/// than a walk that throws inside a launch.
pub fn tree_root_of(db: &dyn Db, session_id: &str) -> String {
    let mut id = session_id.to_string();
    let mut cur = db.get_session(&id).ok().flatten();
    let mut hops = 0;
    while let Some(s) = &cur {
        if s.kind != SessionKind::Subagent || hops >= MAX_LINEAGE_HOPS {
            break;
        }
        let Some(origin_id) = s.origin_id.clone() else {
            break;
        };
        let Some(origin) = db.get_session(&origin_id).ok().flatten() else {
            break;
        };
        id = origin.id.clone();
        cur = Some(origin);
        hops += 1;
    }
    id
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

struct LeaseSt {
    released: bool,
    bound: Option<String>,
}

enum LeaseKind {
    Real { ledger: Arc<Mutex<CapsState>>, tree_id: String, turn_id: String },
    /// The lease a cap-exempt launch carries (workflows, spec §8). A distinct
    /// no-op object rather than an Option, so every call site binds and
    /// releases unconditionally — the branch that forgets to release is the
    /// one that leaks.
    Exempt,
}

struct LeaseInner {
    kind: LeaseKind,
    st: Mutex<LeaseSt>,
}

/// One taken concurrency slot, released when the child's turn ends.
///
/// `release()` is idempotent, and that is load-bearing rather than defensive:
/// the launch path releases when the child's result settles, and the bus
/// backstop releases when the child's `turn.finished` arrives. Both fire for a
/// normal child, so a second release must be a no-op — a lease that
/// decremented twice would hand the tree a fifth concurrent slot, which is the
/// same bug as having no cap.
#[derive(Clone)]
pub struct SpawnLease {
    inner: Arc<LeaseInner>,
}

impl std::fmt::Debug for SpawnLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnLease")
            .field("tree_id", &self.tree_id())
            .field("turn_id", &self.turn_id())
            .field("released", &self.released())
            .field("bound", &self.session_id())
            .finish()
    }
}

impl SpawnLease {
    /// The tree whose concurrency budget this slot came out of. "" for exempt.
    pub fn tree_id(&self) -> &str {
        match &self.inner.kind {
            LeaseKind::Real { tree_id, .. } => tree_id,
            LeaseKind::Exempt => "",
        }
    }

    /// The spawning turn whose per-turn budget was charged. "" for exempt.
    pub fn turn_id(&self) -> &str {
        match &self.inner.kind {
            LeaseKind::Real { turn_id, .. } => turn_id,
            LeaseKind::Exempt => "",
        }
    }

    pub fn released(&self) -> bool {
        self.inner.st.lock().unwrap().released
    }

    /// The child session this slot is for, once it exists.
    pub fn session_id(&self) -> Option<String> {
        self.inner.st.lock().unwrap().bound.clone()
    }

    /// Point the lease at the child session the moment its id is known, so
    /// the bus backstop can release it if the holder never does. Binding after
    /// release would register a lease nothing will ever clean up — no-op.
    pub fn bind(&self, session_id: &str) {
        let mut st = self.inner.st.lock().unwrap();
        if st.released || session_id.is_empty() || st.bound.as_deref() == Some(session_id) {
            return;
        }
        match &self.inner.kind {
            LeaseKind::Exempt => {
                st.bound = Some(session_id.to_string());
            }
            LeaseKind::Real { ledger, .. } => {
                let old = st.bound.take();
                st.bound = Some(session_id.to_string());
                let mut lg = ledger.lock().unwrap();
                if let Some(old) = old {
                    unbind(&mut lg, &old, &self.inner);
                }
                lg.bound
                    .entry(session_id.to_string())
                    .or_default()
                    .push(Arc::downgrade(&self.inner));
            }
        }
    }

    /// Give the slot back. Idempotent.
    pub fn release(&self) {
        let bound = {
            let mut st = self.inner.st.lock().unwrap();
            if st.released {
                return;
            }
            st.released = true;
            st.bound.clone()
        };
        if let LeaseKind::Real { ledger, tree_id, .. } = &self.inner.kind {
            let mut lg = ledger.lock().unwrap();
            let held = lg.running.get(tree_id).copied().unwrap_or(0);
            // `- 1` guarded rather than assumed: a count that went negative
            // would make the cap silently unenforceable for the process's life.
            if held <= 1 {
                lg.running.remove(tree_id);
            } else {
                lg.running.insert(tree_id.clone(), held - 1);
            }
            if let Some(bound) = bound {
                unbind(&mut lg, &bound, &self.inner);
            }
        }
    }
}

/// The lease a launch that is exempt from the caps carries (workflows).
/// Still tracks `released`/`session_id` state; `bind` after release is a no-op.
pub fn exempt_lease() -> SpawnLease {
    SpawnLease {
        inner: Arc::new(LeaseInner {
            kind: LeaseKind::Exempt,
            st: Mutex::new(LeaseSt { released: false, bound: None }),
        }),
    }
}

fn unbind(state: &mut CapsState, session_id: &str, lease: &Arc<LeaseInner>) {
    if let Some(set) = state.bound.get_mut(session_id) {
        set.retain(|w| w.upgrade().map(|a| !Arc::ptr_eq(&a, lease)).unwrap_or(false));
        if set.is_empty() {
            state.bound.remove(session_id);
        }
    }
}

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

/// Test seam: both caps are injectable so a test need not launch eight of
/// anything.
#[derive(Clone, Copy, Debug, Default)]
pub struct CapLimits {
    pub per_turn: Option<u32>,
    pub concurrent: Option<u32>,
}

#[derive(Default)]
struct CapsState {
    /// turnId → launches this turn has been charged for. Never decremented.
    spawns: HashMap<String, u32>,
    /// treeId → slots currently held.
    running: HashMap<String, u32>,
    /// child sessionId → the leases the bus backstop may release.
    bound: HashMap<String, Vec<Weak<LeaseInner>>>,
}

/// The counters behind both caps.
///
/// In memory ON PURPOSE, like the turn registry: a persisted count would
/// always be a lie after a restart. A restart ends every running turn (they
/// are recovered as `orphaned`), so an empty ledger at boot is the truth.
pub struct SpawnCaps {
    pub per_turn: u32,
    pub concurrent: u32,
    ledger: Arc<Mutex<CapsState>>,
    detach: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl SpawnCaps {
    pub fn new() -> Self {
        Self::with_limits(CapLimits::default())
    }

    pub fn with_limits(limits: CapLimits) -> Self {
        SpawnCaps {
            per_turn: limits.per_turn.unwrap_or(MAX_SPAWNS_PER_TURN),
            concurrent: limits.concurrent.unwrap_or(MAX_TREE_CONCURRENT),
            ledger: Arc::new(Mutex::new(CapsState::default())),
            detach: Mutex::new(None),
        }
    }

    /// Check both caps and take both slots, or error having taken NEITHER.
    ///
    /// One lock from first read to last write — the port of the TS
    /// synchronous-section atomicity. The per-turn budget is checked first
    /// because it is the one no amount of waiting clears; but it is charged
    /// only on the path that also takes the concurrency slot, so a launch
    /// refused for concurrency has spent nothing.
    pub fn reserve(&self, turn_id: &str, tree_id: &str) -> Result<SpawnLease, BoughError> {
        let mut lg = self.ledger.lock().unwrap();
        let spawned = lg.spawns.get(turn_id).copied().unwrap_or(0);
        if spawned >= self.per_turn {
            return Err(BoughError::spawn_cap(format!(
                "spawn cap reached: this turn has already launched {spawned} subagents, which is the \
                 per-turn limit ({}). Waiting will not clear it — it counts launches, \
                 not running children. Do the rest of the work in this turn, split it across the \
                 children you already have, or hand the fan-out to a workflow \
                 (workflow.start), which has no per-turn cap. Launches that already \
                 started are unaffected.",
                self.per_turn
            )));
        }

        let running = lg.running.get(tree_id).copied().unwrap_or(0);
        if running >= self.concurrent {
            return Err(BoughError::spawn_cap(format!(
                "subagent concurrency cap reached: {running} subagents are already running across this \
                 tree, which is the tree-wide limit ({}) — it counts every branch, \
                 not just this session's own children. Await or join() the ones in flight, then \
                 launch the rest as a second batch. This launch alone was refused; the ones \
                 already running are untouched.",
                self.concurrent
            )));
        }

        lg.spawns.insert(turn_id.to_string(), spawned + 1);
        lg.running.insert(tree_id.to_string(), running + 1);
        Ok(SpawnLease {
            inner: Arc::new(LeaseInner {
                kind: LeaseKind::Real {
                    ledger: self.ledger.clone(),
                    tree_id: tree_id.to_string(),
                    turn_id: turn_id.to_string(),
                },
                st: Mutex::new(LeaseSt { released: false, bound: None }),
            }),
        })
    }

    /// Slots held by one tree, or by every tree when called with `None`.
    pub fn running(&self, tree_id: Option<&str>) -> u32 {
        let lg = self.ledger.lock().unwrap();
        match tree_id {
            Some(t) => lg.running.get(t).copied().unwrap_or(0),
            None => lg.running.values().sum(),
        }
    }

    /// Launches charged to one turn.
    pub fn spawned_in_turn(&self, turn_id: &str) -> u32 {
        self.ledger.lock().unwrap().spawns.get(turn_id).copied().unwrap_or(0)
    }

    /// Wire the ledger to the event stream. Returns the unsubscribe thunk;
    /// replaces any previous attachment (calls its detach first).
    ///
    /// Two jobs, both about not leaking: (1) a dropped lease — `turn.finished`
    /// for the bound session is the authoritative "this child is no longer
    /// running", on every path; (2) the per-turn map — its entries are dead
    /// weight once the spawning turn ends, and without the GC the map grows by
    /// one entry per delegating turn for as long as the server runs.
    pub fn attach_bus(&self, bus: &Arc<Bus>) -> Arc<dyn Fn() + Send + Sync> {
        if let Some(old) = self.detach.lock().unwrap().take() {
            old();
        }
        let ledger = self.ledger.clone();
        let id = bus.subscribe(Arc::new(move |event: &BoughEvent| on_event(&ledger, event)));
        let bus2 = bus.clone();
        let off: Arc<dyn Fn() + Send + Sync> = Arc::new(move || bus2.unsubscribe(id));
        *self.detach.lock().unwrap() = Some(off.clone());
        off
    }

    /// Drop every count. Tests only — production never un-caps a running tree.
    pub fn reset(&self) {
        let mut lg = self.ledger.lock().unwrap();
        lg.spawns.clear();
        lg.running.clear();
        lg.bound.clear();
    }
}

impl Default for SpawnCaps {
    fn default() -> Self {
        Self::new()
    }
}

fn on_event(ledger: &Arc<Mutex<CapsState>>, event: &BoughEvent) {
    if event.r#type != EventType::TurnFinished {
        return;
    }
    // Read loosely, as TS does — a payload missing a key is a no-op, never a
    // parse failure.
    if let Some(session_id) = event.data.get("sessionId").and_then(|v| v.as_str()) {
        // Collect under the lock, release outside it — `release` re-locks the
        // ledger and a held lock here would deadlock.
        let leases: Vec<Arc<LeaseInner>> = {
            let lg = ledger.lock().unwrap();
            lg.bound
                .get(session_id)
                .map(|v| v.iter().filter_map(Weak::upgrade).collect())
                .unwrap_or_default()
        };
        for inner in leases {
            SpawnLease { inner }.release();
        }
    }
    // The turn that did the spawning is over; its budget entry is dead weight.
    if let Some(turn_id) = event.data.get("turnId").and_then(|v| v.as_str()) {
        ledger.lock().unwrap().spawns.remove(turn_id);
    }
}

// ---------------------------------------------------------------------------
// The nesting rule
// ---------------------------------------------------------------------------

/// Refuse detached delegation from inside a subagent turn (spec §7: subagents
/// may delegate one level further, **blocking only**).
///
/// `depth` is `TurnCtx.depth`, which the runner sets to 1 for any subagent or
/// workflow-agent turn — a tier flag, not a hop count.
pub fn assert_may_delegate(
    depth: u8,
    mode: DelegationMode,
    verb: Option<&str>,
) -> Result<(), BoughError> {
    let verb = verb.unwrap_or(match mode {
        DelegationMode::Detached => "spawn()",
        DelegationMode::Blocking => "agent()",
    });
    if mode != DelegationMode::Detached || depth < 1 {
        return Ok(());
    }
    Err(BoughError::http(
        400,
        ErrorKind::Agent,
        format!(
            "{verb} is not available inside a subagent: nested delegation is blocking-only. \
             Use await agent(task, {{name}}) instead — it runs the child to completion and returns \
             its report in-band, so your own report can account for it. A detached child would \
             outlive this turn and keep writing to the shared checkout after your report has \
             already gone upward. Retrying {verb} will fail the same way."
        ),
    ))
}

// ---------------------------------------------------------------------------
// The launch path
// ---------------------------------------------------------------------------

/// Options for [`reserve_spawn`].
#[derive(Clone, Default)]
pub struct ReserveOptions {
    /// How the caller intends to await the child. Gates the nesting rule.
    pub mode: Option<DelegationMode>,
    /// The host-function name for error text. Defaults from `mode`.
    pub verb: Option<String>,
    /// Skip both width caps. Workflows only; the nesting rule still applies.
    pub exempt: bool,
    /// Defaults to the process ledger on `ctx.app.host.caps`. Tests pass
    /// their own.
    pub caps: Option<Arc<SpawnCaps>>,
}

impl ReserveOptions {
    pub fn blocking() -> Self {
        ReserveOptions { mode: Some(DelegationMode::Blocking), ..Default::default() }
    }
    pub fn detached() -> Self {
        ReserveOptions { mode: Some(DelegationMode::Detached), ..Default::default() }
    }
}

/// Check the nesting rule and take a slot for one launch from this turn.
///
/// Errors with an Agent 400 for a refused nesting, a `SpawnCap` 429 for a cap
/// — both catchable inside the program, both naming which rule and what to do
/// instead.
pub fn reserve_spawn(ctx: &TurnCtx, opts: &ReserveOptions) -> Result<SpawnLease, BoughError> {
    let mode = opts.mode.unwrap_or(DelegationMode::Blocking);
    assert_may_delegate(ctx.depth, mode, opts.verb.as_deref())?;
    if opts.exempt {
        return Ok(exempt_lease());
    }
    let caps = opts.caps.clone().unwrap_or_else(|| ctx.app.host.caps.clone());
    let tree_id = {
        let guard = ctx.app.db.lock().unwrap_or_else(|p| p.into_inner());
        tree_root_of(&*guard, &ctx.session_id)
    };
    caps.reserve(&ctx.turn_id, &tree_id)
}

/// What [`under_lease`] needs of a launch: an id now, and a settlement later.
/// Structural, so this module stays independent of the launch module it
/// guards — the caps are about counting, not about how a subagent is built.
pub trait LeasedLaunch {
    fn session_id(&self) -> String;
    /// Resolves when the launch's result settles, however it settled.
    fn settled(&self) -> BoxFuture<'static, ()>;
}

/// Run one launch under a reservation, releasing the slot on every exit.
///
/// Three endings, and the slot must come back on all of them: the launch
/// errors (release immediately — nothing was started, so nothing is running);
/// the child finishes (release when its result settles, however it settled);
/// or the holder simply forgets, which is what the bus backstop is for. The
/// `bind` in between is what makes that third path possible at all.
pub fn under_lease<T: LeasedLaunch>(
    lease: SpawnLease,
    launch: impl FnOnce() -> Result<T, BoughError>,
) -> Result<T, BoughError> {
    let started = match launch() {
        Ok(s) => s,
        Err(err) => {
            // A refused or failed launch releases what it took and nothing
            // else — in particular it does not disturb the siblings already
            // holding slots.
            lease.release();
            return Err(err);
        }
    };
    lease.bind(&started.session_id());
    let settled = started.settled();
    let on_settle = lease.clone();
    tokio::spawn(async move {
        settled.await;
        on_settle.release();
    });
    Ok(started)
}

/// The whole capped-launch path in one call: nesting rule, both caps, the
/// slot, and its release. This is what a delegation host function calls.
pub fn capped_launch<T: LeasedLaunch>(
    ctx: &TurnCtx,
    opts: &ReserveOptions,
    launch: impl FnOnce() -> Result<T, BoughError>,
) -> Result<T, BoughError> {
    under_lease(reserve_spawn(ctx, opts)?, launch)
}

// ---------------------------------------------------------------------------
// Tests — port of `src/agents/caps.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testkit::{is_cap_error, seed_session, shared_db, turn_ctx_for, SeedOpts};
    use crate::schema::events::EventInput;
    use crate::types::{system_clock, SharedDb};
    use futures::future::Shared;
    use futures::FutureExt;
    use serde_json::json;

    // ---- fixtures -----------------------------------------------------------

    struct FakeLaunch {
        sid: String,
        settled: Shared<BoxFuture<'static, ()>>,
    }

    impl std::fmt::Debug for FakeLaunch {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FakeLaunch").field("sid", &self.sid).finish()
        }
    }

    impl LeasedLaunch for FakeLaunch {
        fn session_id(&self) -> String {
            self.sid.clone()
        }
        fn settled(&self) -> BoxFuture<'static, ()> {
            let s = self.settled.clone();
            async move { s.await }.boxed()
        }
    }

    /// A launch whose child has already finished — the slot comes straight back.
    fn instant_launch(id: &str) -> FakeLaunch {
        FakeLaunch { sid: id.to_string(), settled: futures::future::ready(()).boxed().shared() }
    }

    /// A launch whose child is still running until the test says otherwise.
    fn pending_launch(id: &str) -> (FakeLaunch, tokio::sync::oneshot::Sender<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let settled = async move {
            let _ = rx.await;
        }
        .boxed()
        .shared();
        (FakeLaunch { sid: id.to_string(), settled }, tx)
    }

    /// Let the spawned release tasks run.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    fn caps() -> Arc<SpawnCaps> {
        Arc::new(SpawnCaps::new())
    }

    fn opts_with(mode: DelegationMode, caps: &Arc<SpawnCaps>) -> ReserveOptions {
        ReserveOptions { mode: Some(mode), caps: Some(caps.clone()), ..Default::default() }
    }

    fn turn_finished(bus: &Bus, session_id: &str, turn_id: &str, status: &str) {
        bus.publish(EventInput {
            r#type: EventType::TurnFinished,
            session_id: Some(session_id.to_string()),
            data: json!({ "turnId": turn_id, "sessionId": session_id, "status": status }),
        });
    }

    // ---- the acceptance criterion -------------------------------------------

    #[tokio::test]
    async fn twelve_sequential_launches_eight_succeed_four_refused_the_eight_stand() {
        let db: SharedDb = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        // Each launch's child finishes before the next starts, so at most one
        // is ever in flight and the tree-wide budget never binds — this
        // scenario is about the per-turn cap.
        let mut outcomes: Vec<Result<String, BoughError>> = vec![];
        for i in 0..12 {
            let r = capped_launch(&ctx, &opts_with(DelegationMode::Blocking, &caps), || {
                Ok(instant_launch(&format!("c{i}")))
            })
            .map(|l| format!("report from {}", l.sid));
            settle().await;
            outcomes.push(r);
        }

        let fulfilled: Vec<&String> = outcomes.iter().filter_map(|r| r.as_ref().ok()).collect();
        let rejected: Vec<&BoughError> = outcomes.iter().filter_map(|r| r.as_ref().err()).collect();
        assert_eq!(fulfilled.len() as u32, MAX_SPAWNS_PER_TURN, "eight launches went through");
        assert_eq!(rejected.len(), 4, "four were refused");

        // The successes are intact: the FIRST eight, each with its own child's
        // own report. A refusal that had unwound a sibling would show as a hole.
        let heads: Vec<String> =
            outcomes[..8].iter().map(|r| r.as_ref().unwrap().clone()).collect();
        assert_eq!(
            heads,
            (0..8).map(|i| format!("report from c{i}")).collect::<Vec<_>>()
        );
        assert!(
            outcomes[8..].iter().all(|r| r.as_ref().err().map(is_cap_error).unwrap_or(false)),
            "the refusals are SpawnCapErrors, and they are the LAST four"
        );
        for err in &rejected {
            assert_eq!(err.status(), 429);
            assert!(err.to_string().contains("per-turn limit (8)"), "names WHICH cap: {err}");
            assert!(err.to_string().contains("workflow"), "and the move that resolves it");
        }

        assert_eq!(caps.spawned_in_turn("turn-1"), 8, "only the launches that happened were charged");
        assert_eq!(caps.running(None), 0, "every slot came back when its child finished");
    }

    #[tokio::test]
    async fn twelve_launches_that_all_stay_running_four_take_the_tree_eight_refused() {
        let db = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        // Fired in one synchronous burst: every reservation happens before any
        // child could possibly report. This is the case a database query
        // cannot get right — and the case the reserve Mutex is for.
        let mut started: Vec<tokio::sync::oneshot::Sender<()>> = vec![];
        let mut outcomes: Vec<Result<String, BoughError>> = vec![];
        for i in 0..12 {
            let r = capped_launch(&ctx, &opts_with(DelegationMode::Detached, &caps), || {
                let (l, tx) = pending_launch(&format!("c{i}"));
                started.push(tx);
                Ok(l)
            })
            .map(|l| l.sid);
            outcomes.push(r);
        }

        assert_eq!(
            outcomes.iter().filter(|r| r.is_ok()).count() as u32,
            MAX_TREE_CONCURRENT
        );
        assert_eq!(outcomes.iter().filter(|r| r.is_err()).count(), 8);
        let heads: Vec<String> = outcomes[..4].iter().map(|r| r.clone().unwrap()).collect();
        assert_eq!(heads, vec!["c0", "c1", "c2", "c3"]);
        assert_eq!(started.len(), 4, "a refused launch never ran the launch body at all");

        let refusal = outcomes[4].as_ref().unwrap_err();
        assert!(is_cap_error(refusal));
        assert!(refusal.to_string().contains("concurrency cap reached"));
        assert!(refusal.to_string().contains("tree-wide limit (4)"), "names WHICH cap");
        assert!(refusal.to_string().contains("join()"), "and the move that resolves it");

        // The four in flight are untouched by the eight refusals.
        assert_eq!(caps.running(Some(&root.id)), 4);
        assert_eq!(caps.spawned_in_turn("turn-1"), 4, "refusals charged nothing to the turn");

        for tx in started {
            let _ = tx.send(());
        }
        settle().await;
        assert_eq!(caps.running(None), 0, "the slots come back as the children finish");
    }

    #[tokio::test]
    async fn a_refused_launch_releases_nothing_it_did_not_take() {
        let db = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        let mut held: Vec<tokio::sync::oneshot::Sender<()>> = vec![];
        for i in 0..4 {
            capped_launch(&ctx, &opts_with(DelegationMode::Detached, &caps), || {
                let (l, tx) = pending_launch(&format!("held-{i}"));
                held.push(tx);
                Ok(l)
            })
            .unwrap();
        }
        assert_eq!(caps.running(Some(&root.id)), 4);

        for i in 0..3 {
            let err = capped_launch(&ctx, &opts_with(DelegationMode::Detached, &caps), || {
                Ok(instant_launch(&format!("refused-{i}")))
            })
            .unwrap_err();
            assert!(is_cap_error(&err));
        }

        // Neither budget moved: the refusals did not free a sibling's slot,
        // and they did not spend the per-turn allowance either.
        assert_eq!(caps.running(Some(&root.id)), 4, "the four in flight still hold their slots");
        assert_eq!(caps.spawned_in_turn("turn-1"), 4);

        for tx in held {
            let _ = tx.send(());
        }
        settle().await;
        assert_eq!(caps.running(Some(&root.id)), 0);

        // Four more still fit, because the three refusals cost the turn
        // nothing — the per-turn budget is eight LAUNCHES, not eight attempts.
        for i in 0..4 {
            capped_launch(&ctx, &opts_with(DelegationMode::Blocking, &caps), || {
                Ok(instant_launch(&format!("later-{i}")))
            })
            .unwrap();
            settle().await;
        }
        assert_eq!(caps.spawned_in_turn("turn-1"), 8);
        let ninth = capped_launch(&ctx, &opts_with(DelegationMode::Blocking, &caps), || {
            Ok(instant_launch("ninth"))
        })
        .unwrap_err();
        assert!(is_cap_error(&ninth) && ninth.to_string().contains("per-turn limit"));
    }

    // ---- Mutex-atomic reserve under real concurrency ------------------------

    #[test]
    fn reserve_is_atomic_under_real_thread_concurrency() {
        // 32 threads race one tree with concurrent=4: exactly four takes,
        // whatever the interleaving. This is what the one-lock reserve buys.
        let caps = Arc::new(SpawnCaps::with_limits(CapLimits {
            per_turn: Some(100),
            concurrent: Some(4),
        }));
        let handles: Vec<_> = (0..32)
            .map(|i| {
                let caps = caps.clone();
                std::thread::spawn(move || caps.reserve(&format!("turn-{i}"), "tree").is_ok())
            })
            .collect();
        let takes = handles.into_iter().map(|h| h.join().unwrap()).filter(|took| *took).count();
        assert_eq!(takes, 4, "exactly the tree budget, never more");
        assert_eq!(caps.running(Some("tree")), 4);
    }

    // ---- the tree-wide counter ----------------------------------------------

    #[test]
    fn the_concurrency_budget_is_the_trees_not_the_sessions() {
        let db = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let child = seed_session(&db, SeedOpts::subagent_of(&root.id));
        let grandchild = seed_session(&db, SeedOpts::subagent_of(&child.id));

        {
            let guard = db.lock().unwrap();
            assert_eq!(tree_root_of(&*guard, &root.id), root.id);
            assert_eq!(tree_root_of(&*guard, &child.id), root.id);
            assert_eq!(
                tree_root_of(&*guard, &grandchild.id),
                root.id,
                "every hop lands on the same tree"
            );
        }

        // Three DIFFERENT sessions, three different turns, one budget.
        let from_root = turn_ctx_for(&db, &root.id, "turn-root", 0);
        let from_child = turn_ctx_for(&db, &child.id, "turn-child", 1);
        let from_grandchild = turn_ctx_for(&db, &grandchild.id, "turn-grandchild", 1);

        reserve_spawn(&from_root, &opts_with(DelegationMode::Detached, &caps)).unwrap();
        reserve_spawn(&from_root, &opts_with(DelegationMode::Detached, &caps)).unwrap();
        reserve_spawn(&from_child, &opts_with(DelegationMode::Blocking, &caps)).unwrap();
        reserve_spawn(&from_grandchild, &opts_with(DelegationMode::Blocking, &caps)).unwrap();

        assert_eq!(caps.running(Some(&root.id)), 4);
        assert_eq!(caps.spawned_in_turn("turn-root"), 2, "per-turn counts stay per turn");
        assert_eq!(caps.spawned_in_turn("turn-child"), 1);

        // The fifth is refused wherever in the tree it is launched from —
        // including from a session that has launched nothing at all.
        for c in [&from_root, &from_child, &from_grandchild] {
            let err = reserve_spawn(c, &opts_with(DelegationMode::Blocking, &caps)).unwrap_err();
            assert!(is_cap_error(&err) && err.to_string().contains("tree-wide limit"));
        }

        // A different tree is different work, and holds its own budget.
        let other = seed_session(&db, SeedOpts::default());
        let lease = reserve_spawn(
            &turn_ctx_for(&db, &other.id, "turn-other", 0),
            &opts_with(DelegationMode::Detached, &caps),
        )
        .unwrap();
        assert_eq!(caps.running(Some(&other.id)), 1);
        assert_eq!(caps.running(None), 5, "five running overall, four of them in one tree");
        lease.release();
        assert_eq!(caps.running(Some(&other.id)), 0);
        assert_eq!(
            caps.running(Some(&root.id)),
            4,
            "releasing one tree's slot never touches another's"
        );
    }

    #[test]
    fn a_fork_is_its_own_tree_and_a_dangling_origin_does_not_hang_the_walk() {
        let db = shared_db();
        let root = seed_session(&db, SeedOpts::default());
        let fork = seed_session(&db, SeedOpts {
            kind: Some(SessionKind::Fork),
            origin_id: Some(root.id.clone()),
        });
        let orphan = seed_session(&db, SeedOpts {
            kind: Some(SessionKind::Subagent),
            origin_id: Some("gone-with-the-database".to_string()),
        });
        let mut cur = root.clone();
        for _ in 0..10 {
            cur = seed_session(&db, SeedOpts::subagent_of(&cur.id));
        }

        let guard = db.lock().unwrap();
        assert_eq!(tree_root_of(&*guard, &fork.id), fork.id, "a fork is a branch, not a delegation");
        assert_eq!(tree_root_of(&*guard, &orphan.id), orphan.id);
        // A ten-deep subagent chain still resolves, and resolves to the top.
        assert_eq!(tree_root_of(&*guard, &cur.id), root.id);
    }

    // ---- releasing ----------------------------------------------------------

    #[test]
    fn releasing_twice_frees_one_slot_not_two() {
        let db = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        let first = reserve_spawn(&ctx, &opts_with(DelegationMode::Detached, &caps)).unwrap();
        reserve_spawn(&ctx, &opts_with(DelegationMode::Detached, &caps)).unwrap();
        assert_eq!(caps.running(Some(&root.id)), 2);

        first.release();
        first.release();
        first.release();
        assert!(first.released());
        assert_eq!(caps.running(Some(&root.id)), 1, "the second lease still holds its slot");
    }

    #[tokio::test]
    async fn a_launch_that_throws_releases_the_slot_it_reserved() {
        let db = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        let err = capped_launch::<FakeLaunch>(
            &ctx,
            &opts_with(DelegationMode::Detached, &caps),
            || Err(BoughError::http(400, ErrorKind::Agent, "task must be a non-empty string")),
        )
        .unwrap_err();
        assert_eq!(err.name(), "AgentError");

        assert_eq!(caps.running(Some(&root.id)), 0, "nothing is running, so nothing is held");
        // The per-turn budget IS charged: the model asked for a launch and the
        // answer it gets back is about its own bad call, not about a cap.
        assert_eq!(caps.spawned_in_turn("turn-1"), 1);
    }

    // ---- the bus backstop ---------------------------------------------------

    #[tokio::test]
    async fn a_dropped_lease_is_released_when_the_childs_turn_finishes() {
        let db = shared_db();
        let caps = caps();
        let bus = Arc::new(Bus::new(system_clock()));
        let detach = caps.attach_bus(&bus);
        let root = seed_session(&db, SeedOpts::default());
        let child = seed_session(&db, SeedOpts::subagent_of(&root.id));
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        // A launch whose result nobody will ever settle — a detached child the
        // holder forgot about. Without the backstop this slot is gone forever.
        let (never, _keep) = pending_launch(&child.id);
        let launch = capped_launch(&ctx, &opts_with(DelegationMode::Detached, &caps), || Ok(never))
            .unwrap();
        assert_eq!(caps.running(Some(&root.id)), 1);
        assert_eq!(launch.sid, child.id);

        // An unrelated session's turn finishing must not free it.
        turn_finished(&bus, "someone-else", "t-other", "done");
        assert_eq!(caps.running(Some(&root.id)), 1);

        turn_finished(&bus, &child.id, "t-child", "interrupted");
        assert_eq!(caps.running(Some(&root.id)), 0, "the child's turn ended, slot came back");

        // And the spawning turn's own end clears its per-turn tally.
        assert_eq!(caps.spawned_in_turn("turn-1"), 1);
        turn_finished(&bus, &root.id, "turn-1", "done");
        assert_eq!(caps.spawned_in_turn("turn-1"), 0);

        detach();
        assert_eq!(bus.size(), 0, "attach_bus hands back a working unsubscribe");
    }

    #[tokio::test]
    async fn the_bus_backstop_and_the_result_path_releasing_together_free_one_slot() {
        let db = shared_db();
        let caps = caps();
        let bus = Arc::new(Bus::new(system_clock()));
        let detach = caps.attach_bus(&bus);
        let root = seed_session(&db, SeedOpts::default());
        let kids = [
            seed_session(&db, SeedOpts::subagent_of(&root.id)),
            seed_session(&db, SeedOpts::subagent_of(&root.id)),
        ];
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        let mut finishes = vec![];
        for k in &kids {
            let (l, tx) = pending_launch(&k.id);
            capped_launch(&ctx, &opts_with(DelegationMode::Detached, &caps), || Ok(l)).unwrap();
            finishes.push(tx);
        }
        assert_eq!(caps.running(Some(&root.id)), 2);

        // Both release paths fire for the same child — exactly what happens in
        // production. Idempotence keeps this from freeing a slot nobody held.
        let _ = finishes.remove(0).send(());
        turn_finished(&bus, &kids[0].id, "t0", "done");
        settle().await;

        assert_eq!(caps.running(Some(&root.id)), 1, "one child ended, one slot back");
        let _ = finishes.remove(0).send(());
        settle().await;
        assert_eq!(caps.running(Some(&root.id)), 0);
        detach();
    }

    // ---- the nesting rule ---------------------------------------------------

    #[test]
    fn a_subagent_may_delegate_blocking_and_is_refused_a_detached_spawn() {
        let db = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let child = seed_session(&db, SeedOpts::subagent_of(&root.id));

        // Top level: both modes are available. One level down: blocking only.
        assert_may_delegate(0, DelegationMode::Detached, None).unwrap();
        assert_may_delegate(0, DelegationMode::Blocking, None).unwrap();
        assert_may_delegate(1, DelegationMode::Blocking, None).unwrap();

        let nested = turn_ctx_for(&db, &child.id, "turn-child", 1);
        let err = reserve_spawn(
            &nested,
            &ReserveOptions {
                mode: Some(DelegationMode::Detached),
                verb: Some("spawn()".to_string()),
                caps: Some(caps.clone()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(err.name(), "AgentError");
        assert!(!is_cap_error(&err), "a nesting refusal is not a cap to retry later");
        assert_eq!(err.status(), 400);
        assert!(err.to_string().contains("spawn() is not available inside a subagent"));
        assert!(err.to_string().contains("agent(task, {name})"), "names the verb that does work");

        assert_eq!(caps.running(Some(&root.id)), 0, "a refused nesting took no slot");
        assert_eq!(caps.spawned_in_turn("turn-child"), 0, "and no per-turn budget");

        // The blocking form from the same session goes through.
        reserve_spawn(&nested, &opts_with(DelegationMode::Blocking, &caps)).unwrap();
        assert_eq!(caps.running(Some(&root.id)), 1);
    }

    // ---- the workflow exemption ---------------------------------------------

    #[test]
    fn a_workflows_launches_are_exempt_from_both_caps() {
        let db = shared_db();
        let caps = caps();
        let root = seed_session(&db, SeedOpts::default());
        let ctx = turn_ctx_for(&db, &root.id, "turn-1", 0);

        for _ in 0..20 {
            reserve_spawn(
                &ctx,
                &ReserveOptions {
                    mode: Some(DelegationMode::Blocking),
                    exempt: true,
                    caps: Some(caps.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(caps.running(Some(&root.id)), 0, "an exempt launch is not in the ledger");
        assert_eq!(caps.spawned_in_turn("turn-1"), 0);

        // The exempt lease is still a lease: bindable, releasable, idempotent.
        let lease = exempt_lease();
        lease.bind("child-1");
        assert_eq!(lease.session_id().as_deref(), Some("child-1"));
        lease.release();
        lease.release();
        assert!(lease.released());
        lease.bind("child-2");
        assert_eq!(lease.session_id().as_deref(), Some("child-1"), "bind after release no-ops");
    }

    // ---- injectable limits --------------------------------------------------

    #[test]
    fn the_caps_are_the_specs_numbers_and_are_injectable_for_tests() {
        assert_eq!(MAX_SPAWNS_PER_TURN, 8);
        assert_eq!(MAX_TREE_CONCURRENT, 4);

        let caps = SpawnCaps::new();
        assert_eq!(caps.per_turn, 8);
        assert_eq!(caps.concurrent, 4);

        let tiny = SpawnCaps::with_limits(CapLimits { per_turn: Some(2), concurrent: Some(1) });
        let lease = tiny.reserve("t", "tree").unwrap();
        assert!(is_cap_error(&tiny.reserve("t", "tree").unwrap_err()));
        lease.release();
        tiny.reserve("t", "tree").unwrap();
        let err = tiny.reserve("t", "tree").unwrap_err();
        assert!(is_cap_error(&err) && err.to_string().contains("per-turn limit (2)"));

        tiny.reset();
        assert_eq!(tiny.running(None), 0);
        assert_eq!(tiny.spawned_in_turn("t"), 0);
    }

    #[tokio::test]
    async fn under_lease_binds_the_child_session_so_the_ledger_can_find_the_lease() {
        let caps = SpawnCaps::new();
        let lease = caps.reserve("t", "tree").unwrap();
        assert_eq!(lease.session_id(), None);
        assert_eq!(lease.tree_id(), "tree");
        assert_eq!(lease.turn_id(), "t");

        let observe = lease.clone();
        under_lease(lease, || Ok(instant_launch("kid"))).unwrap();
        assert_eq!(observe.session_id().as_deref(), Some("kid"));
        settle().await;
        assert!(observe.released());
        assert_eq!(caps.running(Some("tree")), 0);
    }
}
