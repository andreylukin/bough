//! The delegation host functions (port of `src/hostfn/delegate.ts`):
//! `agent`, `spawn`, `join`, `adopt`.
//!
//! THE INVARIANT THIS HOLDS: **a blocking child is part of its spawner's turn;
//! a detached one is not.**
//!
//!   - `agent()` and `join()` are work the current turn is doing. Both hang a
//!     cascade on the spawning turn's own cancel token, and both drop it again
//!     the instant they resolve — cascading into a child that already finished
//!     would flip a completed branch to `interrupted` and erase a report that
//!     was already persisted.
//!   - `spawn()` is not. It answers the program with a handle and the child
//!     runs on **regardless of what the spawner does next**. So it never
//!     touches the turn's token. The one thing that does reach it is an
//!     explicit stop of the spawner session, through the registry's cascade
//!     hooks — which exist for exactly this and nothing else.
//!
//! THERE IS NO DONE-GATE. `agent()` returns `{sessionId, title, ok, status,
//! report, changedFiles}` — and no `checkPassed`. `ok` says only whether the
//! child's TURN completed; `status` rides alongside it because "failed" is not
//! one fact: errored, interrupted and orphaned call for different moves from
//! the spawner.
//!
//! WHAT THIS FILE DOES NOT DECIDE. It does not launch — `agents::subagent`
//! owns lineage, naming, the child's ctx and the depth cap, and this module is
//! four ways of awaiting it. The width caps live in `agents::caps`
//! (`capped_launch`), and the completion note in `agents::notes` behind the
//! `deliver` seam.
//!
//! TIERS, AND WHY THEY ARE DERIVED. Which verbs a turn is bridged is a
//! function of where the session sits in the lineage, not of a flag somebody
//! set: a top-level session gets all four, a subagent gets blocking `agent()`
//! (plus `adopt`) only, and a depth-2 subagent or a workflow agent gets none.
//! [`delegation_tier`] reads that off the database, and
//! [`delegation_turn_deps`] pairs each tier with the matching `granted` list,
//! so the prompt sections and the bridge cannot disagree about what exists.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use futures::FutureExt;
use serde_json::Value;

use crate::agents::caps::{capped_launch, DelegationMode, ReserveOptions, SpawnCaps};
use crate::agents::subagent::{
    launch_subagent, subagent_depth, LaunchDeps, SubagentLaunch, SubagentOptions, SubagentResult,
    SubagentResultFuture, MAX_SUBAGENT_DEPTH,
};
use crate::errors::{BoughError, ErrorKind};
use crate::harness::protocol::HostFnName;
use crate::schema::events::{EventInput, EventType};
use crate::schema::parts::{Message, Session, SessionKind};
use crate::turn::queue::TurnRegistry;
use crate::turn::runner::{
    base_host_fns, create_turn_starter, default_program_runner, TurnDeps, BASE_HOST_FNS,
};
use crate::types::{AppCtx, Db, HostFn, HostFns, TurnCtx, TurnStarter};

fn agent_error(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Agent, message)
}

// ---------------------------------------------------------------------------
// Tiers
// ---------------------------------------------------------------------------

/// How much delegation a session may do.
///
///   - `Top`    — root, fork, compaction: all four verbs, detaching included.
///   - `Nested` — a subagent one hop down: blocking `agent()` and `adopt()` only.
///   - `None`   — a depth-2 subagent (the nesting cap) or a workflow agent,
///                which gets no context beyond its prompt and no delegation
///                with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelegationTier {
    Top,
    Nested,
    None,
}

/// Everything a top-level session may call.
///
/// `adopt` is STILL BRIDGED but no longer documented in the prompt. It is a
/// vestige of the era when each subagent had its own workspace and its work
/// had to be taken over; since subagents share their spawner's checkout there
/// is nothing to take — both of its branches now just explain that. Left
/// callable so an old transcript replays unchanged; taken out of the prompt so
/// no round is ever spent on it again.
pub const TOP_LEVEL_DELEGATION: [HostFnName; 4] = [
    HostFnName::Agent,
    HostFnName::Spawn,
    HostFnName::Join,
    HostFnName::Adopt,
];

/// What a subagent may call: blocking only.
///
/// `spawn` and `join` are withheld deliberately. A detached child of a
/// subagent would still be running — and still writing to the shared checkout
/// — after its spawner's report had already been handed upward, mutating a
/// branch the top-level session believes is final.
pub const NESTED_DELEGATION: [HostFnName; 2] = [HostFnName::Agent, HostFnName::Adopt];

/// The verbs a tier is bridged, and therefore the prompt sections it earns.
pub fn delegation_fns_for(tier: DelegationTier) -> &'static [HostFnName] {
    match tier {
        DelegationTier::Top => &TOP_LEVEL_DELEGATION,
        DelegationTier::Nested => &NESTED_DELEGATION,
        DelegationTier::None => &[],
    }
}

/// A session's tier, read off its lineage.
///
/// Derived rather than passed, for the same reason the depth cap is: only the
/// `originId` chain knows how far down a session actually is. `TurnCtx.depth`
/// is a tier flag the runner sets from `kind` alone (1 for any subagent,
/// however deeply nested), so a depth-2 subagent looks identical to a depth-1
/// one there.
///
/// A `workflow_agent` gets `None`: it is given its prompt string and nothing
/// else, and the prompt assembler grants it neither delegation section —
/// bridging a verb it is never told about would be exactly the guess the
/// capability contract forbids.
pub fn delegation_tier(db: &dyn Db, session_id: &str) -> DelegationTier {
    let session = db.get_session(session_id).ok().flatten();
    let Some(session) = session else {
        return DelegationTier::None;
    };
    if session.kind == SessionKind::WorkflowAgent {
        return DelegationTier::None;
    }
    let depth = subagent_depth(db, session_id);
    if depth == 0 {
        DelegationTier::Top
    } else if depth < MAX_SUBAGENT_DEPTH {
        DelegationTier::Nested
    } else {
        DelegationTier::None
    }
}

/// What a child launched from `tier` may itself do. One hop down, never sideways.
pub fn child_tier_of(tier: DelegationTier) -> DelegationTier {
    match tier {
        DelegationTier::Top => DelegationTier::Nested,
        _ => DelegationTier::None,
    }
}

// ---------------------------------------------------------------------------
// The detached register
// ---------------------------------------------------------------------------

/// One live-or-finished detached child, from its spawner's point of view.
pub struct DetachedRecord {
    pub spawner_id: String,
    pub session_id: String,
    pub title: String,
    /// Settles with the child's assembled result; never rejects.
    pub result: SubagentResultFuture,
    /// `join()` (or `adopt()`) took it in-band, so no completion note is owed.
    pub claimed: AtomicBool,
}

/// Detached children, by child session id.
///
/// Memory-only and process-scoped, like the turn registry it sits beside: a
/// server restart orphans the running turn and the record goes with it, which
/// is why `join`'s refusal says so rather than implying the id was wrong. An
/// owned struct rather than a global so a test gets its own and two tests in
/// one file cannot claim each other's children.
///
/// Finished records are kept, not dropped: `join()` after completion is a
/// normal move (spawn three, do other work, claim them all), and a record
/// dropped at completion would turn the ordinary race into an error.
pub struct DetachedSubagents {
    /// Insertion-ordered, so `ids_for` lists the children newest last.
    by_child: Mutex<Vec<Arc<DetachedRecord>>>,
}

impl DetachedSubagents {
    pub fn new() -> Self {
        DetachedSubagents {
            by_child: Mutex::new(Vec::new()),
        }
    }

    pub fn register(
        &self,
        spawner_id: &str,
        session_id: &str,
        title: &str,
        result: SubagentResultFuture,
    ) -> Arc<DetachedRecord> {
        let entry = Arc::new(DetachedRecord {
            spawner_id: spawner_id.to_string(),
            session_id: session_id.to_string(),
            title: title.to_string(),
            result,
            claimed: AtomicBool::new(false),
        });
        let mut list = self.by_child.lock().unwrap();
        list.retain(|r| r.session_id != session_id);
        list.push(entry.clone());
        entry
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<DetachedRecord>> {
        self.by_child
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.session_id == session_id)
            .cloned()
    }

    /// The children this session detached, newest last, as `name (id)` — the
    /// refusal message names them. Both halves, because either one is a legal
    /// thing to pass back to `join()`.
    pub fn ids_for(&self, spawner_id: &str) -> Vec<String> {
        self.by_child
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.spawner_id == spawner_id)
            .map(|r| {
                if r.title.is_empty() || r.title == r.session_id {
                    r.session_id.clone()
                } else {
                    format!("{} ({})", r.title, r.session_id)
                }
            })
            .collect()
    }

    /// Every child of this spawner whose NAME is `name`.
    fn by_name(&self, spawner_id: &str, name: &str) -> Vec<Arc<DetachedRecord>> {
        self.by_child
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.spawner_id == spawner_id && r.title == name)
            .cloned()
            .collect()
    }

    /// Take a child in-band. Idempotent: claiming twice returns the same
    /// record and the same future, because "spawn, join, then join again in a
    /// later round" is a program being careful, not a program being wrong.
    ///
    /// `key` is the session id **or the name `spawn()` was given**. Accepting
    /// only the id made the obvious program wrong: `spawn(task, {name: "x"})`
    /// hands back a name, and `join("x")` is what anyone writes next — it
    /// failed, and the refusal listed bare uuids, so the model's recovery was
    /// to scrape ids out of an error string. The register already knew the
    /// name; it just never looked at it.
    pub fn claim(&self, spawner_id: &str, key: &str) -> Result<Arc<DetachedRecord>, BoughError> {
        // Id first: an id is unambiguous, and a name that happens to equal
        // some other child's id should not shadow it.
        let record = match self.get(key).filter(|r| r.spawner_id == spawner_id) {
            Some(record) => Some(record),
            None => {
                let named = self.by_name(spawner_id, key);
                if named.len() > 1 {
                    // Picking one would be a coin flip over which report the
                    // program receives. Say so instead.
                    let ids: Vec<&str> = named.iter().map(|r| r.session_id.as_str()).collect();
                    return Err(agent_error(
                        400,
                        format!(
                            "join(\"{key}\"): this session spawned {} subagents named \
                             \"{key}\", so the name does not identify one. Join them by id: {}.",
                            named.len(),
                            ids.join(", ")
                        ),
                    ));
                }
                named.into_iter().next()
            }
        };
        let Some(record) = record else {
            let mine = self.ids_for(spawner_id);
            let detail = if !mine.is_empty() {
                format!("Its detached subagents are: {}.", mine.join(", "))
            } else {
                "It has not spawn()ed any — join() only claims a child THIS session \
                 detached with spawn(), and the register is memory-only, so a server \
                 restart clears it. Use agent(task, {name}) to run one to completion."
                    .to_string()
            };
            return Err(agent_error(
                400,
                format!(
                    "join(\"{key}\"): this session has no detached subagent by that \
                     name or id. {detail}"
                ),
            ));
        };
        record.claimed.store(true, Ordering::SeqCst);
        Ok(record)
    }

    pub fn forget(&self, session_id: &str) {
        self.by_child
            .lock()
            .unwrap()
            .retain(|r| r.session_id != session_id);
    }

    pub fn size(&self) -> usize {
        self.by_child.lock().unwrap().len()
    }
}

impl Default for DetachedSubagents {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Options, validated at the bridge
// ---------------------------------------------------------------------------

/// `agent(task, {name})`'s options bag, as it arrives over the string-only
/// wire. Validated here because the bridge IS a boundary: the program is
/// arbitrary model-written JavaScript, so `{name: 42}` is a thing that
/// happens, and it must become a message the next round can act on rather than
/// a branch titled "42".
fn parse_options(verb: &str, opts_json: &str) -> Result<SubagentOptions, BoughError> {
    let text = opts_json.trim();
    let raw: Value = if text.is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(text).map_err(|err| {
            agent_error(
                400,
                format!(
                    "{verb}(task, opts): the options could not be read as JSON ({err}). \
                     Pass a plain object, e.g. {verb}(task, {{name: \"audit auth \
                     handlers\"}})."
                ),
            )
        })?
    };
    if raw.is_null() {
        return Ok(SubagentOptions::default());
    }

    let mut issues: Vec<String> = Vec::new();
    let obj = match &raw {
        Value::Object(o) => Some(o),
        _ => {
            issues.push("opts: expected an object".to_string());
            None
        }
    };
    let mut opts = SubagentOptions::default();
    if let Some(o) = obj {
        match o.get("name") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => opts.name = Some(Value::String(s.clone())),
            Some(_) => issues.push("name: expected a string".to_string()),
        }
        match o.get("model") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => opts.model = Some(s.clone()),
            Some(_) => issues.push("model: expected a string".to_string()),
        }
        match o.get("effort") {
            None | Some(Value::Null) => {}
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(e) => opts.effort = Some(e),
                Err(_) => {
                    issues.push("effort: expected one of low, medium, high, xhigh, max".to_string())
                }
            },
        }
    }
    if !issues.is_empty() {
        return Err(agent_error(
            400,
            format!(
                "{verb}(task, opts): {}. It takes {{name?: string, model?: string, \
                 effort?: \"low\"|\"medium\"|\"high\"|\"xhigh\"|\"max\"}} — always pass a \
                 name, it labels the branch everywhere the user sees it.",
                issues.join("; ")
            ),
        ));
    }
    Ok(opts)
}

// ---------------------------------------------------------------------------
// The host functions
// ---------------------------------------------------------------------------

/// How a launch happens. [`launch_subagent`] satisfies this.
pub type LaunchFn = Arc<
    dyn Fn(&TurnCtx, &str, &SubagentOptions, &LaunchDeps) -> Result<SubagentLaunch, BoughError>
        + Send
        + Sync,
>;

/// Where a detached child's result goes when nobody claimed it: the note
/// pipeline posts it to the spawner as a `[subagent finished]` system note,
/// waking an idle spawner. An unwired seam degrades to "the report is not
/// pushed" rather than to lost work — the branch is still in the tree and the
/// result is still claimable by a later `join()`.
pub type DeliverFn = Arc<dyn Fn(&TurnCtx, &SubagentResult) + Send + Sync>;

/// The seams, so all four verbs are drivable with no worker, no key and no
/// server.
#[derive(Clone, Default)]
pub struct DelegationDeps {
    /// Absent = derived from the session's lineage ([`delegation_tier`]).
    pub tier: Option<DelegationTier>,
    /// Absent = the ctx's registry, which is what the turn runner also
    /// defaults to.
    pub registry: Option<Arc<TurnRegistry>>,
    /// Absent = the register on `ctx.app.host`.
    pub detached: Option<Arc<DetachedSubagents>>,
    /// Absent = [`launch_subagent`].
    pub launch: Option<LaunchFn>,
    /// The width-cap ledger. Absent = the ledger on `ctx.app.host`.
    pub caps: Option<Arc<SpawnCaps>>,
    /// Skip the width caps. Workflows only (their own semaphore bounds the
    /// fan-out). The nesting rule still applies.
    pub exempt: bool,
    /// The child's launch deps — its turn deps, its timeout, its diff seam.
    pub child: Option<Arc<dyn Fn(&TurnCtx) -> LaunchDeps + Send + Sync>>,
    /// See [`DeliverFn`]. Absent = nothing is posted.
    pub deliver: Option<DeliverFn>,
}

struct DelegationInner {
    ctx: TurnCtx,
    registry: Arc<TurnRegistry>,
    detached: Arc<DetachedSubagents>,
    launch: LaunchFn,
    caps: Option<Arc<SpawnCaps>>,
    exempt: bool,
    child: Option<Arc<dyn Fn(&TurnCtx) -> LaunchDeps + Send + Sync>>,
    deliver: Option<DeliverFn>,
}

impl DelegationInner {
    /// WHAT THE CHILD WROTE, for its report. Git cannot answer this: subagents
    /// share this checkout, so a diff at the end is the union of every
    /// concurrent sibling's work. The write verbs know, so the answer comes
    /// from them (`hostfn::files::WriteLog`), per session, read once and
    /// cleared. Composed with whatever a caller already supplied, so a test's
    /// own `child` deps still win — this only fills the field when nothing
    /// else did.
    fn child_deps(&self) -> LaunchDeps {
        let mut base = match &self.child {
            Some(f) => f(&self.ctx),
            None => LaunchDeps::default(),
        };
        if base.changed_files.is_none() {
            let writes = self.ctx.app.host.writes.clone();
            base.changed_files = Some(Arc::new(move |session: &Session| {
                let writes = writes.clone();
                let id = session.id.clone();
                async move { Ok(writes.take(&id)) }.boxed()
            }));
        }
        base
    }

    /// Cascade a stop into a child that is STILL running.
    ///
    /// The guard is the whole point: a blocking child that already resolved
    /// has its report and its outcome persisted on its own branch, and
    /// interrupting it now would flip a finished session to `interrupted` and
    /// overwrite work that was already accepted.
    fn stop_if_running(registry: &TurnRegistry, session_id: &str) {
        if registry.is_running(session_id) {
            registry.interrupt(session_id);
        }
    }

    /// Await a child as part of THIS turn: the spawner's stop reaches it, and
    /// the cascade is dropped the instant the child settles.
    async fn await_as_own_work(
        &self,
        session_id: &str,
        result: SubagentResultFuture,
    ) -> Result<String, BoughError> {
        // A watcher on an already-cancelled token would fire immediately; the
        // TS listener-on-aborted-signal never fires, so neither do we.
        let watcher = if !self.ctx.cancel.is_cancelled() {
            let cancel = self.ctx.cancel.clone();
            let registry = self.registry.clone();
            let sid = session_id.to_string();
            Some(tokio::spawn(async move {
                cancel.cancelled().await;
                Self::stop_if_running(&registry, &sid);
            }))
        } else {
            None
        };
        let outcome = result.await;
        if let Some(w) = watcher {
            w.abort();
        }
        serde_json::to_string(&outcome)
            .map_err(|e| agent_error(500, format!("could not serialize subagent result: {e}")))
    }

    /// Refuse before creating a branch nobody will read.
    fn assert_live(&self, verb: &str) -> Result<(), BoughError> {
        if self.ctx.cancel.is_cancelled() {
            return Err(agent_error(
                409,
                format!(
                    "{verb}(): this turn was interrupted, so nothing was launched. \
                     Anything already done stands; the branches that were running have \
                     been stopped."
                ),
            ));
        }
        Ok(())
    }

    /// Every launch goes through the width caps: the nesting rule, the
    /// per-turn budget and the tree's concurrency slot, with the slot released
    /// when the child settles. A refusal throws `SpawnCapError` naming WHICH
    /// cap, and costs the siblings already running nothing — which is what
    /// makes the documented `Promise.allSettled` fan-out idiom lossless.
    fn capped(
        &self,
        task: &str,
        opts: &SubagentOptions,
        mode: DelegationMode,
        verb: &str,
    ) -> Result<SubagentLaunch, BoughError> {
        let reserve = ReserveOptions {
            mode: Some(mode),
            verb: Some(verb.to_string()),
            exempt: self.exempt,
            caps: self.caps.clone(),
        };
        let deps = self.child_deps();
        capped_launch(&self.ctx, &reserve, || {
            (self.launch)(&self.ctx, task, opts, &deps)
        })
    }

    async fn agent(&self, task: &str, opts_json: &str) -> Result<String, BoughError> {
        self.assert_live("agent")?;
        let opts = parse_options("agent", opts_json)?;
        let child = self.capped(task, &opts, DelegationMode::Blocking, "agent()")?;
        self.await_as_own_work(&child.session_id.clone(), child.result)
            .await
    }

    /// Detached delegation. Returns the handle immediately and deliberately
    /// keeps the child off the turn's token: it survives the spawner's turn
    /// ending, and it survives the spawner's program being wound down. Only an
    /// explicit stop of the spawner session reaches it, through the registry's
    /// cascade hook.
    async fn spawn(&self, task: &str, opts_json: &str) -> Result<String, BoughError> {
        self.assert_live("spawn")?;
        let opts = parse_options("spawn", opts_json)?;
        let child = self.capped(task, &opts, DelegationMode::Detached, "spawn()")?;
        let record = self.detached.register(
            &self.ctx.session_id,
            &child.session_id,
            &child.title,
            child.result.clone(),
        );

        // A Weak hook: the registry stores it, and a strong Arc back at the
        // registry would be a cycle that outlives the child.
        let weak_registry = Arc::downgrade(&self.registry);
        let child_id = child.session_id.clone();
        let hook_id = self.registry.on_interrupt(
            &self.ctx.session_id,
            Arc::new(move || {
                if let Some(registry) = weak_registry.upgrade() {
                    Self::stop_if_running(&registry, &child_id);
                }
            }),
        );

        let result = child.result.clone();
        let deliver = self.deliver.clone();
        let ctx = self.ctx.clone();
        let registry = self.registry.clone();
        let spawner = self.ctx.session_id.clone();
        tokio::spawn(async move {
            let outcome = result.await;
            // Claimed in-band by `join()` — the program already has it, and
            // posting a note as well would tell the spawner the same thing
            // twice.
            if !record.claimed.load(Ordering::SeqCst) {
                if let Some(deliver) = &deliver {
                    deliver(&ctx, &outcome);
                }
            }
            // The hook unregisters itself once the child has settled: a later
            // stop after the child is gone is a no-op rather than a stale
            // cascade.
            registry.off_interrupt(&spawner, hook_id);
        });

        Ok(serde_json::json!({ "sessionId": child.session_id, "title": child.title }).to_string())
    }

    /// Claim a detached child in-band. From this point the child IS this
    /// turn's work, so the spawner's stop reaches it — same containment as the
    /// blocking mode.
    async fn join(&self, session_id: &str) -> Result<String, BoughError> {
        let record = self.detached.claim(&self.ctx.session_id, session_id)?;
        self.await_as_own_work(&record.session_id.clone(), record.result.clone())
            .await
    }

    /// Take over a subagent's session.
    ///
    /// There is nothing to move, and saying so IS the implementation.
    /// Subagents share their spawner's checkout (no per-agent worktrees), so a
    /// child's writes are already in this session's tree the moment it makes
    /// them — the honest answer is the one that stops the model looking for a
    /// merge step that does not exist. What it still does is real: it
    /// validates the lineage, reports the branch's live status, and
    /// re-announces the branch so the rail and the Changes view refresh.
    ///
    /// It deliberately does NOT mark a detached child claimed. A child adopted
    /// while still running would then finish with its report going nowhere.
    async fn adopt(&self, session_id: &str) -> Result<String, BoughError> {
        let child = {
            let db = self.ctx.app.db.lock().unwrap_or_else(|p| p.into_inner());
            db.get_session(session_id)?
        };
        let valid = child.as_ref().is_some_and(|c| {
            c.kind == SessionKind::Subagent
                && c.origin_id.as_deref() == Some(self.ctx.session_id.as_str())
        });
        if !valid {
            return Err(agent_error(
                400,
                format!(
                    "adopt(\"{session_id}\"): that is not a subagent of this session. \
                     adopt() only takes over a branch THIS session spawned; you cannot \
                     adopt a sibling, a grandchild, or an ordinary session."
                ),
            ));
        }
        let child = child.unwrap();
        self.ctx.app.bus.publish(EventInput {
            r#type: EventType::SessionUpdated,
            session_id: Some(child.id.clone()),
            data: serde_json::to_value(&child).unwrap_or_default(),
        });

        let running = self.registry.is_running(&child.id);
        let state = if running {
            "is still running".to_string()
        } else if child.outcome_ok == Some(false) {
            "finished (its turn failed)".to_string()
        } else {
            "finished (its turn completed)".to_string()
        };
        let next = if running {
            if self.detached.get(&child.id).is_some() {
                format!(
                    "await join(\"{}\") to take its report in-band, or end your turn and \
                     let its \"[subagent finished]\" note arrive.",
                    child.id
                )
            } else {
                "wait for the call that started it rather than polling.".to_string()
            }
        } else {
            "read the working tree for what it changed.".to_string()
        };
        Ok(format!(
            "subagent \"{}\" ({}) {state}. It works in THIS session's checkout, so its \
             writes are already here — there is no worktree and nothing to merge. {next}",
            child.title, child.id
        ))
    }
}

/// Build the delegation host functions for one turn.
///
/// Returns only the verbs the tier allows, because **absence is the capability
/// denial**: a name the turn does not bridge is simply not on the host object,
/// and calling it rejects with the bridge's own wording. A `None` tier
/// therefore contributes nothing, not four functions that throw.
pub fn create_delegation_host_fns(ctx: &TurnCtx, deps: DelegationDeps) -> HostFns {
    let tier = deps.tier.unwrap_or_else(|| {
        let db = ctx.app.db.lock().unwrap_or_else(|p| p.into_inner());
        delegation_tier(&*db, &ctx.session_id)
    });
    let mut fns = HostFns::default();
    if tier == DelegationTier::None {
        return fns;
    }

    let inner = Arc::new(DelegationInner {
        ctx: ctx.clone(),
        registry: deps
            .registry
            .unwrap_or_else(|| ctx.app.turn_registry.clone()),
        detached: deps
            .detached
            .unwrap_or_else(|| ctx.app.host.detached.clone()),
        launch: deps.launch.unwrap_or_else(|| Arc::new(launch_subagent)),
        caps: deps.caps,
        exempt: deps.exempt,
        child: deps.child,
        deliver: deps.deliver,
    });

    let agent: HostFn = {
        let inner = inner.clone();
        Arc::new(move |args: Vec<String>| {
            let inner = inner.clone();
            async move {
                let task = args.first().cloned().unwrap_or_default();
                let opts = args.get(1).cloned().unwrap_or_else(|| "{}".to_string());
                inner.agent(&task, &opts).await
            }
            .boxed()
        })
    };
    let adopt: HostFn = {
        let inner = inner.clone();
        Arc::new(move |args: Vec<String>| {
            let inner = inner.clone();
            async move {
                let sid = args.first().cloned().unwrap_or_default();
                inner.adopt(&sid).await
            }
            .boxed()
        })
    };
    fns.agent = Some(agent);
    fns.adopt = Some(adopt);

    if tier == DelegationTier::Top {
        let spawn: HostFn = {
            let inner = inner.clone();
            Arc::new(move |args: Vec<String>| {
                let inner = inner.clone();
                async move {
                    let task = args.first().cloned().unwrap_or_default();
                    let opts = args.get(1).cloned().unwrap_or_else(|| "{}".to_string());
                    inner.spawn(&task, &opts).await
                }
                .boxed()
            })
        };
        let join: HostFn = {
            let inner = inner.clone();
            Arc::new(move |args: Vec<String>| {
                let inner = inner.clone();
                async move {
                    let sid = args.first().cloned().unwrap_or_default();
                    inner.join(&sid).await
                }
                .boxed()
            })
        };
        fns.spawn = Some(spawn);
        fns.join = Some(join);
    }
    fns
}

// ---------------------------------------------------------------------------
// Turn wiring
// ---------------------------------------------------------------------------

/// How delegation is wired into turns, once, at boot.
///
/// `base` is whatever the process already wants on every turn; `extend` is the
/// composition seam for the verb-surface host functions (`workflow`,
/// `schedule`, `ask`, `state`, `artifact`), which bridge their own host
/// functions into the same turn. Both are threaded down into the CHILD's turn
/// deps too, so a subagent's turn behaves like any other turn — same registry,
/// same job reporting, one tier shallower.
#[derive(Clone, Default)]
pub struct DelegationWiring {
    pub base: TurnDeps,
    /// Host functions another task bridges, merged under the delegation verbs.
    pub extend: Option<Arc<dyn Fn(&TurnCtx) -> HostFns + Send + Sync>>,
    /// Launch-level seams for every child: its wall clock, its changed-files
    /// source. The `turn` field is ignored — the recursion owns it.
    pub launch_deps: Option<LaunchDeps>,
    /// Absent = [`launch_subagent`].
    pub launch: Option<LaunchFn>,
    pub detached: Option<Arc<DetachedSubagents>>,
    pub deliver: Option<DeliverFn>,
    /// The width-cap ledger. Absent = the ledger on `ctx.app.host`.
    pub caps: Option<Arc<SpawnCaps>>,
}

/// Lay `over`'s bridged functions on top of `base` — the Rust spelling of the
/// TS object-spread composition in `delegationTurnDeps`.
///
/// THE FIELD LIST IS PINNED BY A DESTRUCTURE, and that is the whole point. The
/// hand-written list this replaced named 18 of the 20 fields and dropped `mcp`
/// and `search` on the floor: `boot.rs` bridged them, this laid every other
/// field over the base, and `HostFns.mcp` stayed `None` for every turn the
/// server ran. What the model saw was `mcp() is not available in this turn`
/// while `granted` said it was and the prompt documented `mcp.call`: a
/// capability written, wired and documented, and reachable from nowhere.
/// `HostFns::get` cannot drift that way because its match is exhaustive; a
/// macro's argument list is checked by nobody, so the fix is to make the
/// compiler count the fields here too. The destructure has no `..`, so adding a
/// field to `HostFns` without naming it below fails to compile rather than
/// becoming a verb that silently disappears.
fn merge_host_fns(base: &mut HostFns, over: HostFns) {
    macro_rules! lay {
        ($($f:ident),* $(,)?) => {{
            let HostFns { $($f),* } = over;
            $( if $f.is_some() { base.$f = $f; } )*
        }};
    }
    lay!(
        bash,
        sh,
        bash_bg,
        bash_output,
        bash_wait,
        bash_kill,
        view,
        patch,
        write,
        search,
        agent,
        spawn,
        join,
        adopt,
        workflow,
        ask,
        state,
        schedule,
        artifact,
        mcp,
        milestone,
        step,
    );
}

/// The `TurnDeps` for a turn at `tier`: the delegation verbs bridged into its
/// programs, and the matching `granted` list so the prompt documents exactly
/// those.
///
/// The two must be built together — this function is the only place that knows
/// both halves. `granted` is the prompt's capability grant and the bridge is
/// the runtime one; a turn told about `spawn()` that cannot call it wastes a
/// round, and a turn that can call one it was never told about will not.
pub fn delegation_turn_deps(tier: DelegationTier, wiring: DelegationWiring) -> TurnDeps {
    let mut deps = wiring.base.clone();
    let mut granted = wiring
        .base
        .granted
        .clone()
        .unwrap_or_else(|| BASE_HOST_FNS.to_vec());
    granted.extend_from_slice(delegation_fns_for(tier));
    deps.granted = Some(granted);

    let w = wiring.clone();
    deps.program_for = Some(Arc::new(move |turn_ctx: &TurnCtx| {
        let mut host = base_host_fns(turn_ctx);
        if let Some(extend) = &w.extend {
            merge_host_fns(&mut host, extend(turn_ctx));
        }
        // Lazily, per launch: a child's turn is a turn like any other, one
        // tier shallower. Building it eagerly would recurse at construction
        // time.
        let child_wiring = w.clone();
        let child: Arc<dyn Fn(&TurnCtx) -> LaunchDeps + Send + Sync> =
            Arc::new(move |_ctx: &TurnCtx| {
                let mut launch_deps = child_wiring.launch_deps.clone().unwrap_or_default();
                launch_deps.turn = Some(delegation_turn_deps(
                    child_tier_of(tier),
                    child_wiring.clone(),
                ));
                launch_deps
            });
        let delegation = create_delegation_host_fns(
            turn_ctx,
            DelegationDeps {
                tier: Some(tier),
                registry: w.base.registry.clone(),
                detached: w.detached.clone(),
                launch: w.launch.clone(),
                caps: w.caps.clone(),
                exempt: false,
                child: Some(child),
                deliver: w.deliver.clone(),
            },
        );
        merge_host_fns(&mut host, delegation);
        default_program_runner(turn_ctx, Some(host))
    }));
    deps
}

/// The `TurnStarter` the server wires at boot, with delegation graded per
/// session.
///
/// One starter per tier, chosen per session at start time. That indirection is
/// required rather than tidy: `TurnDeps.granted` is a fixed list read once
/// inside the runner, so a single starter cannot vary the grant by session —
/// and the grant MUST vary, because a depth-2 subagent and a root are the same
/// code path with different capabilities.
pub fn create_delegating_turn_starter(wiring: DelegationWiring) -> Arc<dyn TurnStarter> {
    Arc::new(DelegatingStarter {
        top: create_turn_starter(delegation_turn_deps(DelegationTier::Top, wiring.clone())),
        nested: create_turn_starter(delegation_turn_deps(DelegationTier::Nested, wiring.clone())),
        none: create_turn_starter(delegation_turn_deps(DelegationTier::None, wiring)),
    })
}

struct DelegatingStarter {
    top: Arc<dyn TurnStarter>,
    nested: Arc<dyn TurnStarter>,
    none: Arc<dyn TurnStarter>,
}

impl TurnStarter for DelegatingStarter {
    fn start_turn(&self, ctx: &AppCtx, session: &Session, message: &Message) {
        let tier = {
            let db = ctx.db.lock().unwrap_or_else(|p| p.into_inner());
            delegation_tier(&*db, &session.id)
        };
        match tier {
            DelegationTier::Top => self.top.start_turn(ctx, session, message),
            DelegationTier::Nested => self.nested.start_turn(ctx, session, message),
            DelegationTier::None => self.none.start_turn(ctx, session, message),
        }
    }
}

// Weak is used by the spawn hook via Arc::downgrade above; keep the import
// honest for non-test builds.
#[allow(unused)]
type _WeakRegistry = Weak<TurnRegistry>;

// ---------------------------------------------------------------------------
// tests — ported from `src/hostfn/delegate.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use bough_llm::LlmError;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::agents::testkit::{seed_session, shared_db, turn_ctx_for, SeedOpts};
    use crate::bus::Bus;
    use crate::errors::BoughError;
    use crate::harness::protocol::HOST_FN_NAMES;
    use crate::prompt::assemble::AssembledPrompt;
    use crate::schema::events::BoughEvent;
    use crate::schema::parts::{Part, Role};
    use crate::turn::testkit::{answering_llm, ok_program};
    use crate::types::{system_clock, LlmBlock, LlmClient, LlmParams, LlmResult, OnText, SharedDb};

    // ---- fixtures -----------------------------------------------------------

    struct Harness {
        db: SharedDb,
        bus: Arc<Bus>,
        events: Arc<Mutex<Vec<BoughEvent>>>,
        registry: Arc<TurnRegistry>,
        detached: Arc<DetachedSubagents>,
    }

    fn harness() -> Harness {
        let db = shared_db();
        let bus = Arc::new(Bus::with_error_hook(system_clock(), Arc::new(|_e, _ev| {})));
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e| sink.lock().unwrap().push(e.clone())));
        Harness {
            db,
            bus,
            events,
            registry: Arc::new(TurnRegistry::new()),
            detached: Arc::new(DetachedSubagents::new()),
        }
    }

    /// A model whose round parks until the test releases it — and that answers
    /// an interrupt the way a real provider client does, by failing with the
    /// abort status. That is what makes an interrupted child land as
    /// `status: "interrupted"` rather than as a turn that quietly succeeded
    /// after the stop.
    struct GatedLlm {
        report: String,
        release: Arc<tokio::sync::Notify>,
        started: tokio::sync::watch::Sender<bool>,
    }

    #[async_trait]
    impl LlmClient for GatedLlm {
        async fn run(
            &self,
            _params: LlmParams,
            _on_text: OnText,
            cancel: CancellationToken,
        ) -> Result<LlmResult, LlmError> {
            let _ = self.started.send(true);
            tokio::select! {
                _ = self.release.notified() => Ok(LlmResult {
                    content: vec![
                        LlmBlock::Text { text: self.report.clone() },
                        LlmBlock::ToolUse {
                            id: "stop-1".to_string(),
                            name: crate::turn::runner::STOP.to_string(),
                            input: json!({}),
                        },
                    ],
                    stop_reason: "tool_use".to_string(),
                    usage: None,
                }),
                _ = cancel.cancelled() => Err(crate::llm::sse::aborted("provider")),
            }
        }
    }

    struct Gate {
        release: Arc<tokio::sync::Notify>,
        started: tokio::sync::watch::Receiver<bool>,
    }

    impl Gate {
        async fn started(&mut self) {
            self.started.wait_for(|v| *v).await.unwrap();
        }
        fn release(&self) {
            self.release.notify_one();
        }
    }

    fn gated_llm(report: &str) -> (Arc<dyn LlmClient>, Gate) {
        let release = Arc::new(tokio::sync::Notify::new());
        let (tx, rx) = tokio::sync::watch::channel(false);
        (
            Arc::new(GatedLlm {
                report: report.to_string(),
                release: release.clone(),
                started: tx,
            }),
            Gate {
                release,
                started: rx,
            },
        )
    }

    /// The spawning turn's ctx, as the runner would have built it, with the
    /// child's scripted model on the app.
    fn spawner_ctx(h: &Harness, session_id: &str, llm: Arc<dyn LlmClient>) -> TurnCtx {
        let mut ctx = turn_ctx_for(&h.db, session_id, "turn-spawner", 0);
        let mut app = ctx.app.clone();
        app.db = h.db.clone();
        app.bus = h.bus.clone();
        app.llm = Some(llm);
        app.turn_registry = h.registry.clone();
        ctx.app = app;
        ctx.message_id = seed_supervisor(&h.db, session_id);
        ctx
    }

    /// A pending supervisor message, as a spawning turn would have.
    fn seed_supervisor(db: &SharedDb, session_id: &str) -> String {
        let id = Uuid::new_v4().to_string();
        db.lock()
            .unwrap()
            .create_message(Message {
                id: id.clone(),
                session_id: session_id.to_string(),
                role: Role::Supervisor,
                parts: vec![],
                pending: true,
                created_at: 1_002,
            })
            .unwrap();
        id
    }

    /// Child-turn deps that never touch a worker and never share the global
    /// registry.
    fn child_turn_deps(h: &Harness) -> TurnDeps {
        TurnDeps {
            registry: Some(h.registry.clone()),
            program: Some(ok_program()),
            assemble: Some(Arc::new(|_input| AssembledPrompt {
                system: "SYSTEM".to_string(),
                system_volatile: String::new(),
                sections: vec![],
                shas: vec![],
            })),
            outage_delay_ms: Some(0),
            report_error: Some(Arc::new(|_e, _s| {})),
            ..Default::default()
        }
    }

    /// The delegation deps every test shares: own registry, own register, no
    /// worker.
    fn delegation_deps(h: &Harness) -> DelegationDeps {
        let deps = child_turn_deps(h);
        DelegationDeps {
            registry: Some(h.registry.clone()),
            detached: Some(h.detached.clone()),
            caps: Some(Arc::new(SpawnCaps::new())),
            child: Some(Arc::new(move |_ctx| LaunchDeps {
                turn: Some(deps.clone()),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    /// The JSON a delegation verb hands back to the program, re-inflated.
    async fn call(f: &HostFn, args: Vec<&str>) -> Result<String, BoughError> {
        f(args.into_iter().map(str::to_string).collect()).await
    }

    fn parse(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    async fn until_idle(registry: &TurnRegistry, session_id: &str) {
        while registry.is_running(session_id) {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    // ---- the blocking round trip --------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_runs_a_child_to_completion_and_returns_its_report_in_band() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let ctx = spawner_ctx(
            &h,
            &spawner.id,
            answering_llm("Renamed foo to bar; tests pass."),
        );
        let mut deps = delegation_deps(&h);
        let child_deps = child_turn_deps(&h);
        deps.child = Some(Arc::new(move |_ctx| LaunchDeps {
            turn: Some(child_deps.clone()),
            changed_files: Some(Arc::new(|_s| {
                async { Ok(vec!["src/thing.ts".to_string()]) }.boxed()
            })),
            ..Default::default()
        }));
        let host = create_delegation_host_fns(&ctx, deps);

        let result = parse(
            &call(
                host.agent.as_ref().unwrap(),
                vec![
                    "Rename foo to bar in src/thing.ts.",
                    r#"{"name":"renamer"}"#,
                ],
            )
            .await
            .unwrap(),
        );

        // The four fields, and the report is the child's own final text.
        assert!(result["sessionId"].is_string());
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["report"], json!("Renamed foo to bar; tests pass."));
        assert_eq!(result["changedFiles"], json!(["src/thing.ts"]));
        // The done-gate is gone: there is no harness-verified check, so there
        // is no field that could claim one passed.
        assert!(
            !result.as_object().unwrap().contains_key("checkPassed"),
            "no acceptance gate, and no field implying one"
        );
        // Carried alongside `ok` so "failed" is not one undifferentiated fact.
        assert_eq!(result["status"], json!("done"));
        assert_eq!(
            result["title"],
            json!("renamer"),
            "the spawner's name labels the branch"
        );

        // It really was a subagent branch of this session, and it really
        // finished.
        let child_id = result["sessionId"].as_str().unwrap();
        {
            let db = h.db.lock().unwrap();
            let child = db.get_session(child_id).unwrap().unwrap();
            assert_eq!(child.kind, SessionKind::Subagent);
            assert_eq!(child.origin_id.as_deref(), Some(spawner.id.as_str()));
            assert_eq!(
                child.origin_message_id.as_deref(),
                Some(ctx.message_id.as_str())
            );
            assert_eq!(
                db.thread_for(child_id).unwrap().len(),
                2,
                "its task and its own answer, nothing else"
            );
        }
        assert!(!h.registry.is_running(child_id));
        // Blocking work leaves nothing detached behind for a later join() to
        // find.
        assert_eq!(h.detached.size(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blocking_child_that_fails_reports_why_without_throwing_at_the_spawner() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let failing: Arc<dyn LlmClient> = {
            struct Failing;
            #[async_trait]
            impl LlmClient for Failing {
                async fn run(
                    &self,
                    _p: LlmParams,
                    _t: OnText,
                    _c: CancellationToken,
                ) -> Result<LlmResult, LlmError> {
                    Err(LlmError::with("provider is on fire", 400, None))
                }
            }
            Arc::new(Failing)
        };
        let ctx = spawner_ctx(&h, &spawner.id, failing);
        let mut deps = delegation_deps(&h);
        let mut child = child_turn_deps(&h);
        child.max_round_retries = Some(0);
        deps.child = Some(Arc::new(move |_ctx| LaunchDeps {
            turn: Some(child.clone()),
            ..Default::default()
        }));
        let host = create_delegation_host_fns(&ctx, deps);

        let result = parse(
            &call(
                host.agent.as_ref().unwrap(),
                vec!["Do the impossible.", r#"{"name":"doomed"}"#],
            )
            .await
            .unwrap(),
        );
        assert_eq!(result["ok"], json!(false));
        assert_eq!(
            result["status"],
            json!("error"),
            "distinguishable from an interrupt or an orphan"
        );
        assert!(
            result["report"].as_str().unwrap().contains("on fire"),
            "{result}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_refuses_options_it_cannot_use_naming_the_shape_it_wants() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let ctx = spawner_ctx(&h, &spawner.id, answering_llm("x"));
        let host = create_delegation_host_fns(&ctx, delegation_deps(&h));

        let bad_name = call(
            host.agent.as_ref().unwrap(),
            vec!["do it", r#"{"name":42}"#],
        )
        .await
        .unwrap_err();
        assert_eq!(bad_name.name(), "AgentError");
        assert!(bad_name.to_string().contains("name"), "{bad_name}");
        assert!(
            bad_name.to_string().contains("always pass a name"),
            "{bad_name}"
        );

        let not_json = call(host.agent.as_ref().unwrap(), vec!["do it", "not json"])
            .await
            .unwrap_err();
        assert_eq!(not_json.name(), "AgentError");
        assert!(
            not_json.to_string().contains("could not be read as JSON"),
            "{not_json}"
        );

        let db = h.db.lock().unwrap();
        assert_eq!(
            db.list_sessions().unwrap().len(),
            1,
            "no branch was created for a refused launch"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_launch_refused_at_a_cap_fails_alone_naming_the_cap() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let ctx = spawner_ctx(&h, &spawner.id, answering_llm("done"));
        // One launch per turn, so the second is refused without waiting for
        // eight.
        let mut deps = delegation_deps(&h);
        deps.caps = Some(Arc::new(SpawnCaps::with_limits(
            crate::agents::caps::CapLimits {
                per_turn: Some(1),
                concurrent: Some(4),
            },
        )));
        let host = create_delegation_host_fns(&ctx, deps);

        let first = parse(
            &call(
                host.agent.as_ref().unwrap(),
                vec!["The one launch this turn gets.", r#"{"name":"first"}"#],
            )
            .await
            .unwrap(),
        );
        assert_eq!(first["ok"], json!(true));

        let refused = call(
            host.agent.as_ref().unwrap(),
            vec!["One too many.", r#"{"name":"second"}"#],
        )
        .await
        .unwrap_err();
        assert!(refused.to_string().contains("per-turn limit"), "{refused}");
        // The refusal cost the sibling nothing: its branch and its report
        // still stand.
        let db = h.db.lock().unwrap();
        let first_child = db
            .get_session(first["sessionId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(first_child.outcome_ok, Some(true));
        assert_eq!(
            db.sessions_by_origin(&spawner.id).unwrap().len(),
            1,
            "and no branch was created"
        );
    }

    // ---- detaching ----------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_returns_before_the_child_finishes_and_the_child_runs_on() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let (llm, mut gate) = gated_llm("swept the handlers");
        let ctx = spawner_ctx(&h, &spawner.id, llm);
        let delivered: Arc<Mutex<Vec<SubagentResult>>> = Arc::new(Mutex::new(vec![]));
        let sink = delivered.clone();
        let mut deps = delegation_deps(&h);
        deps.deliver = Some(Arc::new(move |_c, r| sink.lock().unwrap().push(r.clone())));
        let host = create_delegation_host_fns(&ctx, deps);

        let handle = parse(
            &call(
                host.spawn.as_ref().unwrap(),
                vec!["Sweep the handlers.", r#"{"name":"sweeper"}"#],
            )
            .await
            .unwrap(),
        );
        assert!(handle["sessionId"].is_string());
        assert_eq!(handle["title"], json!("sweeper"));
        assert!(
            !handle.as_object().unwrap().contains_key("report"),
            "the handle is a promise of work, not its result"
        );

        // The claim this test exists for: spawn() answered while the child's
        // first round is still in flight. Gated, so this is a fact and not a
        // race that usually wins.
        let child_id = handle["sessionId"].as_str().unwrap().to_string();
        gate.started().await;
        assert!(
            h.registry.is_running(&child_id),
            "the child is still mid-turn"
        );
        assert_eq!(
            delivered.lock().unwrap().len(),
            0,
            "and nothing has been reported yet"
        );

        gate.release();
        let result = h.detached.get(&child_id).unwrap().result.clone().await;
        assert!(result.ok);
        assert_eq!(result.report, "swept the handlers");
        // Unclaimed, so the report is handed to the note deliverer rather
        // than being dropped.
        until_idle(&h.registry, &child_id).await;
        for _ in 0..50 {
            if !delivered.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(
            delivered
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.session_id.clone())
                .collect::<Vec<_>>(),
            vec![child_id]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_claims_a_detached_childs_result_in_band_so_no_note_is_owed() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let (llm, mut gate) = gated_llm("audit complete: two missing error paths");
        let ctx = spawner_ctx(&h, &spawner.id, llm);
        let delivered: Arc<Mutex<Vec<SubagentResult>>> = Arc::new(Mutex::new(vec![]));
        let sink = delivered.clone();
        let mut deps = delegation_deps(&h);
        deps.deliver = Some(Arc::new(move |_c, r| sink.lock().unwrap().push(r.clone())));
        let host = create_delegation_host_fns(&ctx, deps);

        let handle = parse(
            &call(
                host.spawn.as_ref().unwrap(),
                vec!["Audit the handlers.", r#"{"name":"auditor"}"#],
            )
            .await
            .unwrap(),
        );
        let child_id = handle["sessionId"].as_str().unwrap().to_string();
        gate.started().await;

        // Invoke the HostFn directly: its BoxFuture is 'static, so it can be
        // spawned without borrowing `host` or `child_id`.
        let claimed = host.join.as_ref().unwrap()(vec![child_id.clone()]);
        let claimed = tokio::spawn(claimed);
        // Let the join claim before the release.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        gate.release();
        let result = parse(&claimed.await.unwrap().unwrap());

        assert_eq!(result["sessionId"], json!(child_id));
        assert_eq!(result["ok"], json!(true));
        assert_eq!(
            result["report"],
            json!("audit complete: two missing error paths")
        );
        assert!(h
            .detached
            .get(&child_id)
            .unwrap()
            .claimed
            .load(Ordering::SeqCst));

        // Give the completion chain its ticks: the note must NOT be posted,
        // because the program already has the report in hand.
        let _ = h.detached.get(&child_id).unwrap().result.clone().await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(
            delivered.lock().unwrap().is_empty(),
            "a claimed result is not also announced"
        );

        // Joining again is a program being careful, not a program being wrong.
        let again = parse(
            &call(host.join.as_ref().unwrap(), vec![&child_id])
                .await
                .unwrap(),
        );
        assert_eq!(again["sessionId"], json!(child_id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_takes_the_name_spawn_was_given_not_only_the_id_it_returned() {
        // The program anyone writes: spawn with a name, join by that name.
        // It used to 400, and the refusal listed bare uuids, so the model's
        // recovery was to scrape ids out of an error string.
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let ctx = spawner_ctx(&h, &spawner.id, answering_llm("mapped the turn lifecycle"));
        let host = create_delegation_host_fns(&ctx, delegation_deps(&h));

        let handle = parse(
            &call(
                host.spawn.as_ref().unwrap(),
                vec!["Explore bough-core.", r#"{"name":"core-explorer"}"#],
            )
            .await
            .unwrap(),
        );
        let child_id = handle["sessionId"].as_str().unwrap().to_string();

        let joined = parse(
            &call(host.join.as_ref().unwrap(), vec!["core-explorer"])
                .await
                .unwrap(),
        );
        assert_eq!(joined["sessionId"], json!(child_id), "same child");
        assert_eq!(joined["report"], json!("mapped the turn lifecycle"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_by_a_name_two_children_share_refuses_rather_than_picking_one() {
        // Choosing between them would be a coin flip over which report the
        // program receives, so the ambiguity is the answer.
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let ctx = spawner_ctx(&h, &spawner.id, answering_llm("done"));
        let host = create_delegation_host_fns(&ctx, delegation_deps(&h));
        for _ in 0..2 {
            call(
                host.spawn.as_ref().unwrap(),
                vec!["Explore.", r#"{"name":"twin"}"#],
            )
            .await
            .unwrap();
        }

        let err = call(host.join.as_ref().unwrap(), vec!["twin"])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("2 subagents named"), "{err}");
        assert!(
            err.to_string().contains("Join them by id"),
            "names the way through: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_refuses_an_id_this_session_never_detached_and_says_what_join_is_for() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let ctx = spawner_ctx(&h, &spawner.id, answering_llm("x"));
        let host = create_delegation_host_fns(&ctx, delegation_deps(&h));

        let err = call(host.join.as_ref().unwrap(), vec!["no-such-session"])
            .await
            .unwrap_err();
        assert_eq!(err.name(), "AgentError");
        assert!(err.to_string().contains("has not spawn()ed any"), "{err}");
        assert!(
            err.to_string().contains("restart clears it"),
            "says why an id can go missing: {err}"
        );

        // A child of a DIFFERENT session is not joinable here either.
        let other = seed_session(&h.db, SeedOpts::default());
        let other_ctx = spawner_ctx(&h, &other.id, answering_llm("done"));
        let other_host = create_delegation_host_fns(&other_ctx, delegation_deps(&h));
        let theirs = parse(
            &call(
                other_host.spawn.as_ref().unwrap(),
                vec!["Their work.", r#"{"name":"theirs"}"#],
            )
            .await
            .unwrap(),
        );
        let theirs_id = theirs["sessionId"].as_str().unwrap().to_string();
        let _ = h.detached.get(&theirs_id).unwrap().result.clone().await;
        let foreign = call(host.join.as_ref().unwrap(), vec![&theirs_id])
            .await
            .unwrap_err();
        assert_eq!(foreign.name(), "AgentError");
    }

    // ---- containment --------------------------------------------------------

    // QUARANTINED, not abandoned — github.com/andreylukin/bough/issues/49.
    // This fails on ubuntu CI roughly one run in three, and never locally:
    // 100+ single runs on macOS, three full-suite runs, and a run with every
    // core saturated all pass. It is also not this assertion's fault. Ubuntu
    // job durations are bimodal with no overlap — 86/105/113/127s passing against
    // 960/964/966/971s when it fails — and the bough-core suite itself reports
    // `finished in 31.04s` versus `905.29s` for the SAME 1413 tests. Something
    // makes the whole suite ~29x slower (bimodal ⇒ a blocking timeout, not
    // gradual CPU contention) and this test's timing is merely the first thing
    // to break under it. Chase the 905s, not the assert; un-ignore when the
    // suite runs at its normal speed on Linux.
    //
    // The invariant it covers — a detached child survives its spawner's turn
    // being interrupted — stays covered from the other direction by
    // `an_explicit_stop_of_the_spawner_session_does_cascade_to_a_detached_child`.
    #[ignore = "flaky on ubuntu CI only, and only when the suite runs 29x slow"]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_spawning_turns_interrupt_reaches_a_blocking_child_not_a_detached_one() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let (blocking_llm, mut blocking_gate) = gated_llm("never gets here");
        let (detached_llm, mut detached_gate) = gated_llm("finished on my own");
        let turn = CancellationToken::new();

        // Two ctxs over one session, so the two children get different
        // scripted models while sharing the spawning turn's token — the thing
        // under test.
        let mut blocking_ctx = spawner_ctx(&h, &spawner.id, blocking_llm);
        blocking_ctx.cancel = turn.clone();
        let mut detached_ctx = spawner_ctx(&h, &spawner.id, detached_llm);
        detached_ctx.cancel = turn.clone();
        let blocking_host = create_delegation_host_fns(&blocking_ctx, delegation_deps(&h));
        let detached_host = create_delegation_host_fns(&detached_ctx, delegation_deps(&h));

        let handle = parse(
            &call(
                detached_host.spawn.as_ref().unwrap(),
                vec!["Long detached work.", r#"{"name":"detached"}"#],
            )
            .await
            .unwrap(),
        );
        let detached_id = handle["sessionId"].as_str().unwrap().to_string();
        let agent_fn = blocking_host.agent.clone().unwrap();
        let pending = tokio::spawn(async move {
            agent_fn(vec![
                "Blocking work.".to_string(),
                r#"{"name":"blocking"}"#.to_string(),
            ])
            .await
        });
        blocking_gate.started().await;
        detached_gate.started().await;

        let blocking_id = {
            let db = h.db.lock().unwrap();
            db.sessions_by_origin(&spawner.id)
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .find(|id| *id != detached_id)
                .unwrap()
        };
        assert!(h.registry.is_running(&blocking_id));
        assert!(h.registry.is_running(&detached_id));

        // The user stops the spawning turn.
        turn.cancel();
        let result = parse(&pending.await.unwrap().unwrap());

        assert_eq!(
            result["status"],
            json!("interrupted"),
            "the blocking child is this turn's work"
        );
        assert_eq!(result["ok"], json!(false));
        {
            let db = h.db.lock().unwrap();
            assert_eq!(
                db.get_session(&blocking_id).unwrap().unwrap().outcome_ok,
                Some(false)
            );
        }

        // …and the detached one is not. It is still running, and it still
        // finishes.
        assert!(
            h.registry.is_running(&detached_id),
            "a detached child survives its spawner's turn being interrupted"
        );
        detached_gate.release();
        let survivor = h.detached.get(&detached_id).unwrap().result.clone().await;
        assert_eq!(
            survivor.status,
            crate::agents::subagent::SubagentStatus::Done
        );
        assert_eq!(survivor.report, "finished on my own");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_explicit_stop_of_the_spawner_session_does_cascade_to_a_detached_child() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let (llm, mut gate) = gated_llm("never finishes");
        let ctx = spawner_ctx(&h, &spawner.id, llm);
        let host = create_delegation_host_fns(&ctx, delegation_deps(&h));

        let handle = parse(
            &call(
                host.spawn.as_ref().unwrap(),
                vec!["Runaway work.", r#"{"name":"runaway"}"#],
            )
            .await
            .unwrap(),
        );
        let child_id = handle["sessionId"].as_str().unwrap().to_string();
        gate.started().await;
        assert!(h.registry.is_running(&child_id));

        // The registry cascade, which is a detached child's only stop path
        // from above. The spawner's own turn is not even running here — hooks
        // fire regardless, because a detached child outlives the turn that
        // started it.
        h.registry.interrupt(&spawner.id);

        let result = h.detached.get(&child_id).unwrap().result.clone().await;
        assert_eq!(
            result.status,
            crate::agents::subagent::SubagentStatus::Interrupted
        );
        assert!(!result.ok);

        // And the hook unregisters itself once the child has settled: a
        // second stop after the child is gone is a no-op rather than a stale
        // cascade.
        for _ in 0..100 {
            if !h.registry.interrupt(&spawner.id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(!h.registry.interrupt(&spawner.id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_verb_called_after_the_turn_was_interrupted_refuses_instead_of_branching() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let mut ctx = spawner_ctx(&h, &spawner.id, answering_llm("x"));
        let turn = CancellationToken::new();
        turn.cancel();
        ctx.cancel = turn;
        let host = create_delegation_host_fns(&ctx, delegation_deps(&h));

        for f in [host.agent.as_ref().unwrap(), host.spawn.as_ref().unwrap()] {
            let err = call(f, vec!["do it", "{}"]).await.unwrap_err();
            assert_eq!(err.name(), "AgentError");
            assert!(err.to_string().contains("interrupted"), "{err}");
        }
        let db = h.db.lock().unwrap();
        assert_eq!(
            db.list_sessions().unwrap().len(),
            1,
            "no branch was created"
        );
    }

    // ---- adopt --------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_validates_the_lineage_and_says_there_is_nothing_to_merge() {
        let h = harness();
        let spawner = seed_session(&h.db, SeedOpts::default());
        let ctx = spawner_ctx(&h, &spawner.id, answering_llm("done"));
        let host = create_delegation_host_fns(&ctx, delegation_deps(&h));

        let result = parse(
            &call(
                host.agent.as_ref().unwrap(),
                vec!["Do the thing.", r#"{"name":"worker"}"#],
            )
            .await
            .unwrap(),
        );
        let child_id = result["sessionId"].as_str().unwrap().to_string();

        let before = h.events.lock().unwrap().len();
        let text = call(host.adopt.as_ref().unwrap(), vec![&child_id])
            .await
            .unwrap();
        assert!(text.contains("worker"), "{text}");
        assert!(text.contains("nothing to merge"), "{text}");
        assert!(text.contains("finished"), "{text}");
        assert!(
            h.events.lock().unwrap()[before..].iter().any(|e| {
                e.r#type == EventType::SessionUpdated
                    && e.session_id.as_deref() == Some(child_id.as_str())
            }),
            "the branch is re-announced so the rail and the Changes view refresh"
        );

        // A session that is not this one's subagent is not adoptable.
        let err = call(host.adopt.as_ref().unwrap(), vec![&spawner.id])
            .await
            .unwrap_err();
        assert_eq!(err.name(), "AgentError");
        assert!(
            err.to_string().contains("not a subagent of this session"),
            "{err}"
        );
    }

    // ---- tiers --------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn the_tier_follows_the_lineage_and_the_bridge_follows_the_tier() {
        let h = harness();
        let root = seed_session(&h.db, SeedOpts::default());
        let one = seed_session(&h.db, SeedOpts::subagent_of(&root.id));
        let two = seed_session(&h.db, SeedOpts::subagent_of(&one.id));
        let wf = seed_session(
            &h.db,
            SeedOpts {
                kind: Some(SessionKind::WorkflowAgent),
                origin_id: None,
            },
        );

        {
            let db = h.db.lock().unwrap();
            assert_eq!(delegation_tier(&*db, &root.id), DelegationTier::Top);
            assert_eq!(delegation_tier(&*db, &one.id), DelegationTier::Nested);
            assert_eq!(
                delegation_tier(&*db, &two.id),
                DelegationTier::None,
                "the nesting cap"
            );
            assert_eq!(delegation_tier(&*db, &wf.id), DelegationTier::None);
            assert_eq!(
                delegation_tier(&*db, "no such session"),
                DelegationTier::None
            );
        }

        let bridged = |session_id: &str| {
            let ctx = spawner_ctx(&h, session_id, answering_llm("x"));
            let fns = create_delegation_host_fns(&ctx, delegation_deps(&h));
            let mut names: Vec<&str> = vec![];
            if fns.adopt.is_some() {
                names.push("adopt");
            }
            if fns.agent.is_some() {
                names.push("agent");
            }
            if fns.join.is_some() {
                names.push("join");
            }
            if fns.spawn.is_some() {
                names.push("spawn");
            }
            names
        };

        assert_eq!(bridged(&root.id), vec!["adopt", "agent", "join", "spawn"]);
        assert_eq!(
            bridged(&one.id),
            vec!["adopt", "agent"],
            "a subagent delegates blocking only"
        );
        assert_eq!(
            bridged(&two.id),
            Vec::<&str>::new(),
            "absence is the denial — the bridge rejects with the prompt's own wording"
        );
        assert_eq!(bridged(&wf.id), Vec::<&str>::new());

        assert_eq!(child_tier_of(DelegationTier::Top), DelegationTier::Nested);
        assert_eq!(child_tier_of(DelegationTier::Nested), DelegationTier::None);
    }

    #[test]
    fn each_tiers_grant_matches_what_it_can_actually_call() {
        // The prompt gate and the bridge are built from one list, per tier, so
        // a section documenting spawn() cannot reach a session that has no
        // spawn().
        let granted = |tier: DelegationTier| {
            delegation_turn_deps(tier, DelegationWiring::default())
                .granted
                .unwrap()
        };

        let mut top = BASE_HOST_FNS.to_vec();
        top.extend_from_slice(&TOP_LEVEL_DELEGATION);
        assert_eq!(granted(DelegationTier::Top), top);
        let mut nested = BASE_HOST_FNS.to_vec();
        nested.extend_from_slice(&NESTED_DELEGATION);
        assert_eq!(granted(DelegationTier::Nested), nested);
        assert_eq!(granted(DelegationTier::None), BASE_HOST_FNS.to_vec());

        for tier in [
            DelegationTier::Top,
            DelegationTier::Nested,
            DelegationTier::None,
        ] {
            let delegation: Vec<HostFnName> = granted(tier)
                .into_iter()
                .filter(|f| {
                    matches!(
                        f,
                        HostFnName::Agent
                            | HostFnName::Spawn
                            | HostFnName::Join
                            | HostFnName::Adopt
                    )
                })
                .collect();
            assert_eq!(delegation, delegation_fns_for(tier).to_vec());
        }
    }

    #[test]
    fn the_merge_carries_every_bridged_verb_the_process_wires() {
        // WHAT THIS CAUGHT, and why it is asserted over the protocol's own name
        // list rather than over a list written here: `merge_host_fns` named 18 of
        // the 20 fields, so `mcp` and `search` were bridged by `boot.rs` and
        // dropped here. `mcp.call` then failed in every turn with "not available
        // in this turn" while the grant said it was granted and the prompt
        // documented the call, for the nine days between the verb landing and
        // this test. A second list to keep in step would reproduce the bug it is
        // here to stop.
        let f: HostFn = Arc::new(|_| futures::future::ready(Ok(String::new())).boxed());
        let over = HostFns {
            bash: Some(f.clone()),
            sh: Some(f.clone()),
            bash_bg: Some(f.clone()),
            bash_output: Some(f.clone()),
            bash_wait: Some(f.clone()),
            bash_kill: Some(f.clone()),
            view: Some(f.clone()),
            patch: Some(f.clone()),
            write: Some(f.clone()),
            search: Some(f.clone()),
            agent: Some(f.clone()),
            spawn: Some(f.clone()),
            join: Some(f.clone()),
            adopt: Some(f.clone()),
            workflow: Some(f.clone()),
            ask: Some(f.clone()),
            state: Some(f.clone()),
            schedule: Some(f.clone()),
            artifact: Some(f.clone()),
            mcp: Some(f.clone()),
            milestone: Some(f.clone()),
            step: Some(f.clone()),
        };
        let mut base = HostFns::default();
        merge_host_fns(&mut base, over);
        for name in HOST_FN_NAMES {
            let typed = HostFnName::parse(name).expect("a wire name is a typed name");
            assert!(
                base.get(typed).is_some(),
                "{name}() was bridged and the merge dropped it"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_wired_starter_picks_the_tier_from_the_session_it_is_starting() {
        let h = harness();
        let root = seed_session(&h.db, SeedOpts::default());
        let started: Arc<Mutex<Vec<Vec<HostFnName>>>> = Arc::new(Mutex::new(vec![]));
        // `assemble` is the runner's prompt seam: what it is handed IS the
        // grant this turn resolved, which is what makes the starter's tier
        // choice observable.
        let sink = started.clone();
        let start = create_delegating_turn_starter(DelegationWiring {
            base: TurnDeps {
                registry: Some(h.registry.clone()),
                program: Some(ok_program()),
                assemble: Some(Arc::new(move |input| {
                    sink.lock().unwrap().push(input.granted.clone());
                    AssembledPrompt {
                        system: String::new(),
                        system_volatile: String::new(),
                        sections: vec![],
                        shas: vec![],
                    }
                })),
                outage_delay_ms: Some(0),
                report_error: Some(Arc::new(|_e, _s| {})),
                ..Default::default()
            },
            ..Default::default()
        });

        let ctx = spawner_ctx(&h, &root.id, answering_llm("hello"));
        // A user message so the turn has something to answer.
        let post_user = |session_id: &str, at: i64| {
            h.db.lock()
                .unwrap()
                .create_message(Message {
                    id: Uuid::new_v4().to_string(),
                    session_id: session_id.to_string(),
                    role: Role::User,
                    parts: vec![Part::Text {
                        text: "hi".to_string(),
                    }],
                    pending: false,
                    created_at: at,
                })
                .unwrap()
        };
        let root_msg = post_user(&root.id, 2_000);
        start.start_turn(&ctx.app, &root, &root_msg);
        // The turn is detached; wait for it to release the session.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        until_idle(&h.registry, &root.id).await;

        assert_eq!(started.lock().unwrap().len(), 1);
        assert!(
            started.lock().unwrap()[0].contains(&HostFnName::Spawn),
            "a root is granted detached delegation"
        );

        // The same starter, a subagent session: blocking only.
        let sub = seed_session(&h.db, SeedOpts::subagent_of(&root.id));
        let sub_msg = post_user(&sub.id, 2_100);
        start.start_turn(&ctx.app, &sub, &sub_msg);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        until_idle(&h.registry, &sub.id).await;

        assert_eq!(started.lock().unwrap().len(), 2);
        let sub_granted = started.lock().unwrap()[1].clone();
        assert!(sub_granted.contains(&HostFnName::Agent));
        assert!(
            !sub_granted.contains(&HostFnName::Spawn),
            "a subagent may not detach work"
        );
    }
}
