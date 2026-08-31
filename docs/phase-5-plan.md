# Phase 5 — many agents, the leader, graph ops: design and work breakdown

REQUIREMENTS §1 (dormancy), §2 (the leader is an ordinary agent row with a plugin set in its
scope), §3 (edges, digests, `step_refs`, membership is derived, mail fan-out), §4 (graph ops),
§5 (mail routing, wake urgencies, the standing invariant, per-agent scope), §8 (drift-watch's
claim-rejection signal, leader-attributed reconsolidation), §10 (`worker-fork`), §11 (strip,
focus pane, click-to-focus), §17 Phase 5.

Phases 0-4 built one resident on one lane. Everything in this phase is what happens when there is
more than one of them: mail has to choose recipients, lanes have to be born and merged and put to
sleep, one agent has to be able to do things the others cannot, and the surface has to show a
population rather than a conversation.

The standing decisions of Phases 2, 3 and 4 carry unchanged (`docs/phase-2-plan.md` §7,
`docs/phase-3-plan.md` §6, `docs/phase-4-plan.md` §6). Six of them shape this phase directly:

- **P2-D16**: urgency and drain scheduling are modules inside `agent-loop`, not a `wake-scheduler`
  row. Dormancy therefore cannot be "a different scheduler"; it must attach to the loop through an
  extension point (2.2 below).
- **P2-D8**: the live inbox is a cache of the `inbox/spliced` fold. Dormancy follows the same
  shape: the dormant set is a cache of an `agent/dormancy` fold, never a stamped column.
- **P3-D15**: `Agent::deliver` appends `mail/delivered` FIRST and then splices the message carrying
  that step's seq. The router does not re-implement delivery; it chooses recipients and calls that
  method once per recipient, which is what makes per-agent consumption free.
- **P3-D11**: the focus pane and the strip read the LEDGER BODY by step-type name, never the crate
  that writes it. The dormancy glyph and the claim cards follow it: `tui-strip` and `tui-focus`
  gain no dependency on `dormancy` or `claims`.
- **P4-D2**: a system pass runs under a synthetic wake id. Graph ops do the same (`op:<uuid7>`).
- **Phase 4 already built inheritance digests**: `DigestRequest.parents` + `digest::inheritance_id`
  seal a digest of the parent chain FOR a child, with `src_trajs` naming the parents. Split and bud
  spend that seam rather than growing one.

---

## 1. Crates

Eight new plugin crates, each `bough-plugin-<name>` under `plugins/` (AGENTS.md layout), plus named
edits to nine existing crates.

| package | path | role | provides | injects | row |
|---|---|---|---|---|---|
| `bough-plugin-mail-router` | `plugins/mail-router` | **Definition + Provider** of `ctx.mail`: the `Envelope` vocabulary, the pure ref matcher, fan-out through `Agent::deliver`, the unsorted queue and its sink slot, `link_ref`, the leader-question entry point. | `mail` | required `ledger`, `agents` | `mail` in `bough-base` |
| `bough-plugin-dormancy` | `plugins/dormancy` | **Definition + Provider** of `ctx.dormancy`: the dormant fold, the `agent/wake-request` admission listener, reactivation + drain arming, `/sleep` and `/wake`. | `dormancy` | required `ledger`, `agents`; optional `commands` | `dormancy` in `bough-base` |
| `bough-plugin-claims` | `plugins/claims` | **Definition + Provider** of `ctx.claims` and the one Consumer that may only PROPOSE (the global `propose_claim` tool): open-claim query, accept / edit / reject, requirement→pin, lane birth through `ctx.graph`. | `claims` | required `ledger`, `agents`, `graph`; optional `tools`, `commands` | `claims` in `bough-base` |
| `bough-plugin-graph-ops` | `plugins/graph-ops` | **Definition + Provider** of `ctx.graph` (a Consumer of `ledger`, `agents`, `rollups`, `mail`): split, merge, bud, fork, undo; the pure routing planner; the leader question. | `graph` | required `ledger`, `agents`, `rollups`, `mail` | `graph` in `bough-base` |
| `bough-plugin-leader` | `plugins/leader` | **Definition + Provider** of `ctx.leader`: binds the set to ONE agent named by config, registers the persona section in that agent's scope, owns unsorted adoption, requirement drafting, reconsolidation attribution and timeline curation. | `leader` | required `agents`, `ledger`, `mail`, `graph`, `claims`, `projection`; optional `reconsolidation` | `leader` in `bough-tui-app` (group `leader.set`) |
| `bough-plugin-tool-leader` | `plugins/tool-leader` | **Consumers**: the leader-scoped model-facing tools (`adopt_unsorted`, `draft_requirement`, `propose_claim` — the shadowing one —, `propose_structure`, `note_timeline`). Registered with `ToolScope::Agent(leader)`, owned by this row. | — | required `leader`, `tools`, `claims`, `graph` | `tool.leader` in `bough-tui-app` (group `leader.set`) |
| `bough-plugin-lane-scope` | `plugins/lane-scope` | **Consumer**: per-lane persona section and `tools.restrict` from config; the second scope consumer, and V6's subject. | — | required `agents`, `tools`, `projection` | `lane.scope` in `bough-base` |
| `bough-plugin-worker-fork` | `plugins/worker-fork` | **Provider** for `WorkerKind::Fork`: a child agent on a real ledger fork of the parent's trajectory, with the parent's assembled prefix pinned byte-identically. | — (registers on `workers`) | required `workers`, `agents`, `ledger`, `projection`, `tools` | `worker.fork` in `bough-base` |

**Edits to existing crates** (each owned by exactly one work package, named there):

- `plugins/agents` — the `agent/wake-request` waterfall + `WakeAdmission` vocabulary, three new
  `WakeCause` variants, the `agent/dormancy` step type is NOT here (dormancy owns it), and one
  invariant clause. (WP-2)
- `plugins/agent-loop`, `plugins/agent-loop-scripted` — dispatch the admission waterfall at the one
  wake-opening choke point (`LoopDriver::spawn_wake`, and the scripted driver's equivalent); the
  standing-invariant check gains its dormancy exception. (WP-2)
- `plugins/rollups`, `plugins/rollups-summarizer`, `plugins/rollups-none` — ONE additive field,
  `DigestRequest.reconcile`, and the `recon:` id namespace it selects. (WP-3)
- `plugins/projection` (Definition) + `plugins/projection-assembler` (the sole `Projector`
  implementor) — one new method, `pin_prefix`, and the `PrefixSource` it records. (WP-6)
- `plugins/tool-workers` — a third row, `tool-fork`. (WP-6)
- `plugins/tui-focus` — the paragraph join (the field bug), claim cards, the branch picker. (WP-7)
- `plugins/tui-strip` — the dormant glyph and status word. (WP-7)
- `plugins/drift-watch` — `signals::claim_rejection` activates. (WP-4)
- `plugins/residents` — many bootstrap lanes; the roster no longer implies a catch-up wake. (WP-8)

Nothing in Phase 5 touches `crates/bough-kernel`, `plugins/ledger` (schema or vocabulary),
`plugins/ledger-sqlite`, `plugins/ledger-memory`, `plugins/tools`, `plugins/llm*`,
`plugins/actions`, `plugins/tui-shell` or `plugins/tui-search`. The ledger already carries
`edges`, `EdgeKind::Merge`, `Fork`/`ForkOutcome`, `Connected`, `AgentRow.routing_refs`,
`AgentRow.wake_classes`, `step_refs` and the three `claim/*` and two `pin/*` step types: Phase 1
built them for exactly this phase, and Phase 5 spends them.

---

## 2. Public API

### 2.1 The mail seam — `plugins/mail-router/src/…`

```rust
// lib.rs
pub struct Mail;
impl ServiceKey for Mail {
    type Value = MailHandle;
    const NAME: &'static str = "mail";
}

#[derive(Clone)]
pub struct MailHandle(pub Arc<MailInner>);

/// One piece of mail as a PRODUCER hands it over: no recipient, ever. Choosing recipients is
/// this crate's whole job (§3: "mail delivery is the one eager step").
#[derive(Clone, Debug)]
pub struct Envelope {
    pub from: Sender,
    /// §5's two urgencies. `Wake` is what may reactivate a dormant agent, gated by `refs`.
    pub class: MailClass,
    pub subject: String,
    pub summary: String,
    pub text: String,
    pub cites: Vec<Cite>,
    /// The routing key. A wake CLASS is a ref in the `class:` namespace (P5-D3).
    pub refs: BTreeSet<Ref>,
    pub at: DateTime<Utc>,
}

/// What one `route` did. `delivered` is per-agent: one `mail/delivered` step, one seq, one
/// consumption state each (§3, §5).
#[derive(Clone, Debug)]
pub struct RouteReport {
    pub matched: Vec<AgentName>,
    pub delivered: Vec<(AgentName, InboxReceipt)>,
    /// `Some` iff `matched` was empty: the `mail/unrouted` step on the unsorted trajectory.
    pub unsorted: Option<StepId>,
    /// `true` iff an unsorted sink was mounted and took it as ordinary mail.
    pub adopted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkReport {
    pub agent: AgentName,
    pub added: BTreeSet<Ref>,
    /// ALWAYS 0. §5: "a late-added routing ref starts mail delivery from link time, with earlier
    /// history reachable by query, never queued as backlog." Named in the report so the rule is
    /// asserted rather than assumed.
    pub backfilled: usize,
    /// Trajectories the new ref now reaches through `connected()`, for the caller to show.
    pub now_connected: Vec<TrajId>,
}

/// A question only Andrey (through the leader) can settle.
#[derive(Clone, Debug)]
pub struct Question {
    pub asked_by: &'static str,
    pub about: String,
    pub options: Vec<String>,
    pub cites: Vec<Cite>,
    pub refs: BTreeSet<Ref>,
    pub at: DateTime<Utc>,
}

impl MailHandle {
    /// Fan out. Appends nothing when `matched` is empty except the unsorted step.
    pub async fn route(&self, env: Envelope) -> Result<RouteReport, MailError>;

    /// Add routing refs to an agent's row. No backfill, by construction.
    pub async fn link_ref(&self, agent: &AgentName, refs: BTreeSet<Ref>, at: DateTime<Utc>)
        -> Result<LinkReport, MailError>;
    pub async fn unlink_ref(&self, agent: &AgentName, refs: BTreeSet<Ref>, at: DateTime<Utc>)
        -> Result<LinkReport, MailError>;

    /// §4's "ambiguous routing becomes a leader question, never a guess". Appends
    /// `leader/question` to the unsorted trajectory and routes it at `MailClass::Wake` with the
    /// `class:ask` ref, so it reactivates a dormant leader.
    pub async fn ask_leader(&self, q: Question) -> Result<StepId, MailError>;

    /// The unsorted queue, oldest first: what the leader's `adopt_unsorted` reads.
    pub async fn unsorted(&self, limit: usize) -> Result<Vec<Step>, MailError>;
    /// Mark unsorted items adopted by an agent (appends `mail/adopted` and re-routes them to it).
    pub async fn adopt(&self, to: &AgentName, steps: &[StepId], at: DateTime<Utc>)
        -> Result<Vec<InboxReceipt>, MailError>;

    /// Who receives unsorted mail as live mail. An EFFECT: the `leader` row installs it in its
    /// own fiber, so moving the leader set moves the sink with it (SWAP).
    pub async fn unsorted_sink(&self, ctx: &Context, sink: Arc<dyn UnsortedSink>)
        -> Result<EffectHandle, PluginError>;
}

#[async_trait::async_trait]
pub trait UnsortedSink: Send + Sync + 'static {
    fn agent(&self) -> AgentName;
}

/// PURE, and the ONE place §3's fan-out rule is written: every agent whose `routing_refs`
/// intersect the envelope's refs, in NAME order. Never "the best match"; never one winner.
pub fn recipients(refs: &BTreeSet<Ref>, rows: &[AgentRow]) -> Vec<AgentName>;

/// PURE: the wake classes an envelope carries — its refs in the `class:` namespace (P5-D3).
pub fn wake_classes_of(refs: &BTreeSet<Ref>) -> BTreeSet<String>;
```

Events:

```rust
/// `mail/route` — WATERFALL over the routing DECISION. The extension point §0.2 names for the
/// mail domain: a later row (a ward, a collector policy) may add or remove recipients. The
/// crate's own listener is the ref matcher and it is NOT prepended: it seeds the decision before
/// dispatch, so a listener that skips `next()` short-circuits to a decision that already exists.
pub struct MailRoute;
impl WaterfallEvent for MailRoute {
    const NAME: &'static str = "mail/route";
    type Value = RouteDecision;
}
#[derive(Clone)]
pub struct RouteDecision { pub env: Arc<Envelope>, pub to: Vec<AgentName> }

/// `mail/routed` — EMIT, post-delivery. The TUI's toast and the invariant read it.
pub struct MailRouted;
impl EmitEvent for MailRouted {
    const NAME: &'static str = "mail/routed";
    type Payload = RouteReport;
}
```

Step types (declared through `LedgerHandle::declare_step_types`, owner `mail-router`):

| type | class | body |
|---|---|---|
| `mail/unrouted` | Evidence | `{ from: Ref, subject: String, summary: String, refs: Vec<Ref> }` — on the unsorted trajectory only |
| `mail/adopted` | Evidence | `{ unrouted: StepId, to: AgentName, by: Attribution }` |
| `leader/question` | Thought | `{ asked_by: String, about: String, options: Vec<String> }` — a question is not truth (§16) |
| `agent/routing` | Evidence | `{ agent: AgentName, added: Vec<Ref>, removed: Vec<Ref>, by: Attribution }` |

Config: `MailConfig { unsorted_traj: String, unsorted_limit: usize, deliver_to_dormant: bool }`.
`deliver_to_dormant` defaults `true` and is not a dormancy switch: §5 says mail QUEUES for a
dormant agent, so delivery happens and the WAKE is what dormancy suppresses.

Invariant (`invariant.rs`): every `mail/unrouted` step has zero `step_refs` matching any live
`agents.routing_refs` AT THE TIME IT WAS WRITTEN (recomputed against the row history is not
possible, so the check is the weaker, honest one: an unrouted step whose refs match a row that
existed before it is reported); and every routed envelope produced exactly one `mail/delivered`
step per recipient, never zero and never two.

### 2.2 Dormancy — `plugins/dormancy/src/…`

The one loop-facing addition of this phase. §1 says a dormant agent gets **no ticks and no wakes**;
`agent/pre-step` is too late (it rejects a claim inside an already-open durable wake), so admission
happens before a wake opens, at the single choke point every loop Provider already has.

```rust
// in `plugins/agents/src/events.rs` (WP-2's edit to the Definition)

/// `agent/wake-request` — WATERFALL, dispatched by EVERY loop Provider immediately before it
/// opens a wake and appends `wake/start`. A listener that returns `Defer` stops the wake from
/// existing at all: no `wake/start`, no claim, no step. The default (no listener) is `Open`.
pub struct AgentWakeRequest;
impl WaterfallEvent for AgentWakeRequest {
    const NAME: &'static str = "agent/wake-request";
    type Value = WakeAdmission;
}

#[derive(Clone, Debug)]
pub struct WakeAdmission {
    pub agent: AgentName,
    pub id: AgentId,
    pub kind: WakeKind,
    pub cause: WakeCause,
    /// What would trigger this wake, read from the inbox without claiming it.
    pub trigger: Option<TriggerFacts>,
    pub at: DateTime<Utc>,
    pub decision: Admit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Admit {
    Open,
    /// `by` names the row that deferred, for the toast and the ledger-free explanation.
    Defer { by: &'static str, reason: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerFacts {
    pub message: MessageId,
    pub from_andrey: bool,
    pub class: MailClass,
    /// The message's refs, so a listener never re-reads the inbox.
    pub refs: BTreeSet<Ref>,
    pub mail_seq: Option<Seq>,
}

/// Three variants added to the existing enum (WP-2). `Mail` and `Andrey` were implicit before:
/// the driver opened those wakes with no cause at all, which an admission listener cannot read.
pub enum WakeCause {
    CatchUp,
    Schedule(&'static str),
    Mail { class: MailClass },   // NEW
    Andrey,                      // NEW
    Reactivated,                 // NEW: the drain armed by a reactivation
}
```

```rust
// plugins/dormancy/src/lib.rs
pub struct Dormancy;
impl ServiceKey for Dormancy {
    type Value = DormancyHandle;
    const NAME: &'static str = "dormancy";
}

#[derive(Clone)]
pub struct DormancyHandle(pub Arc<DormancyInner>);

impl DormancyHandle {
    /// The cache of the fold. Never a database read on the admission path.
    pub fn is_dormant(&self, agent: &AgentName) -> bool;
    pub fn dormant(&self) -> Vec<AgentName>;

    /// Put a lane to sleep. Appends `agent/dormancy { dormant: true }`; cites what justified it.
    pub async fn sleep(&self, req: SleepRequest) -> Result<DormancyChange, DormancyError>;

    /// Wake a lane up. Appends `agent/dormancy { dormant: false }` and, if unconsumed ordinary
    /// mail exists, requests ONE drain wake — §5's standing invariant is what drains the backlog.
    pub async fn wake_up(&self, req: WakeUpRequest) -> Result<DormancyChange, DormancyError>;

    /// Rebuild one agent's state from the ledger fold. Called at activation for every row.
    pub async fn reload(&self, agent: &AgentName) -> Result<bool, DormancyError>;
}

#[derive(Clone, Debug)]
pub struct SleepRequest {
    pub agent: AgentName,
    pub reason: String,
    pub by: Attribution,      // bough_plugin_rollups::Attribution
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct WakeUpRequest {
    pub agent: AgentName,
    pub cause: ReactivateCause,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReactivateCause { Andrey, WakeClass, Command }

#[derive(Clone, Debug, PartialEq)]
pub struct DormancyChange {
    pub agent: AgentName,
    pub dormant: bool,
    pub step: StepId,
    /// `Some` when reactivation armed the drain the standing invariant demands.
    pub drain: Option<WakeId>,
}

/// PURE, and the whole of §1's activation rule. Total over its inputs; no clock, no ledger.
pub fn admits(
    dormant: bool,
    kind: WakeKind,
    trigger: Option<&TriggerFacts>,
    wake_classes: &BTreeSet<String>,
) -> Decision;

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Not dormant, or a wake that must run anyway.
    Admit,
    /// Dormant, and this trigger reactivates: the caller writes the step, then admits.
    Reactivate(ReactivateCause),
    /// Dormant: no wake. Ordinary mail stays queued and unconsumed on purpose.
    Defer(&'static str),
}
```

Step type (owner `dormancy`): `agent/dormancy`, `ClassRule::Either`,
`{ dormant: bool, reason: String, by: Attribution, cause: Option<ReactivateCause> }`. It is
appended to the agent's OWN trajectory, so the fold is one `StepQuery { trajs: [traj], kinds:
["agent/dormancy"], order: SeqDesc, limit: 1 }` per agent at activation.

Commands (registered only when `commands` is provided): `/sleep <agent> [reason]`,
`/wake <agent>`, `/dormant` (list).

Edits to the loop (WP-2), stated so both Providers agree:

- `agent-loop`: `LoopDriver::spawn_wake` becomes `async` and dispatches `agent/wake-request`
  before minting the wake id. `Admit::Defer` returns `None` — exactly what the `stopping` guard
  already returns — so `notify`, `wake_now` and `arm_drain` need no new branches.
- `agent-loop`'s standing invariant gains one clause: `standing_invariant_holds(unconsumed,
  drain_scheduled, dormant)` is `dormant || unconsumed == 0 || drain_scheduled`. Without it a
  dormant agent with queued mail is a permanent invariant violation, and `enforce_standing_invariant`
  would arm a drain the admission listener then defers, forever.
- `agent-loop-scripted` dispatches the same waterfall before each scripted wake, so every test
  that proves dormancy proves it for both loops.

### 2.3 Graph ops — `plugins/graph-ops/src/…`

```rust
pub struct Graph;
impl ServiceKey for Graph {
    type Value = GraphHandle;
    const NAME: &'static str = "graph";
}

#[derive(Clone)]
pub struct GraphHandle(pub Arc<dyn GraphOps>);

#[async_trait::async_trait]
pub trait GraphOps: Send + Sync + 'static {
    /// PURE with respect to the world: what an op WOULD write, and every refusal with a reason.
    async fn plan(&self, req: &OpRequest) -> Result<OpPlan, GraphError>;
    /// Execute. Every op is one transaction in the sense that a failure leaves NOTHING half-done
    /// that a later op would trip over: the cited op step is appended LAST (P5-D8).
    async fn apply(&self, req: &OpRequest) -> Result<OpOutcome, GraphError>;
    /// §4's undo rules. `Pointers` for an unused split, `Merge` for a lived-in one.
    async fn undo(&self, req: &UndoRequest) -> Result<OpOutcome, GraphError>;
}

#[derive(Clone, Debug)]
pub enum OpRequest {
    Split(SplitRequest),
    Merge(MergeRequest),
    Bud(BudRequest),
    Fork(ForkRequest),
}

#[derive(Clone, Debug)]
pub struct SplitRequest {
    pub parent: AgentName,
    /// `None` ⇒ the parent's head, resolved to the last seq outside an open wake (P5-D7).
    pub at_seq: Option<Seq>,
    /// Exactly two. Each names the new lane and the refs it takes with it.
    pub children: Vec<ChildSpec>,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ChildSpec {
    /// `None` ⇒ a headless branch: a trajectory and an ancestor edge, no `agents` row (§4 fork).
    pub agent: Option<AgentName>,
    pub traj: TrajId,
    pub routing_refs: BTreeSet<Ref>,
    pub wake_classes: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct BudRequest {
    pub parent: AgentName,
    /// The PAST point. Mandatory: a bud whose point is the head is a split (§4).
    pub at_seq: Seq,
    pub child: ChildSpec,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ForkRequest {
    pub parent: AgentName,
    pub at_seq: Option<Seq>,
    pub traj: TrajId,
    pub reason: String,
    pub by: Attribution,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct MergeRequest {
    /// ANDREY'S CHOICE. Never inferred; a `None` here is a leader question, not a default.
    pub survivor: AgentName,
    pub absorbed: AgentName,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct UndoRequest {
    /// The `graph/split` or `graph/bud` step being undone.
    pub of: StepId,
    pub by: Attribution,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpPlan {
    pub kind: OpKind,
    pub at_seq: Seq,
    pub new_trajs: Vec<TrajId>,
    pub edges: Vec<(TrajId, TrajId, EdgeKind)>,
    pub digests: Vec<DigestPlan>,
    pub routing: RoutingPlan,
    /// Non-empty ⇒ `apply` refuses and `ask_leader` is the caller's next move.
    pub questions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigestPlan { pub traj: TrajId, pub parents: Vec<TrajId>, pub reconcile: bool }

#[derive(Clone, Debug, PartialEq)]
pub struct OpOutcome {
    pub kind: OpKind,
    /// The cited op step (`graph/split` | `graph/merge` | `graph/bud` | `graph/undo`).
    pub step: StepId,
    pub trajs: Vec<TrajId>,
    pub edges: usize,
    pub digests: Vec<RollupId>,
    pub rows_written: Vec<AgentName>,
    pub rows_deleted: Vec<AgentName>,
    /// `Undo::Pointers` did no summarising; `Undo::Merge` produced a reconciliation digest.
    pub undo_shape: Option<UndoShape>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UndoShape { Pointers, Merge }
```

The routing planner, pure (`route.rs`) — the reason ambiguity can never be guessed:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct RoutingPlan {
    /// Per new/surviving row: the refs it ends up with.
    pub assign: Vec<(AgentName, BTreeSet<Ref>)>,
    /// Refs the parent keeps.
    pub keep: BTreeSet<Ref>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RoutingVerdict { Settled(RoutingPlan), Ambiguous(Vec<Ambiguity>) }

#[derive(Clone, Debug, PartialEq)]
pub struct Ambiguity { pub r#ref: Ref, pub claimed_by: Vec<AgentName> }

/// PURE. A ref claimed by two children, or a ref of the parent claimed by none while the parent
/// is being absorbed, is AMBIGUOUS — never resolved by order, name, or "most specific".
pub fn plan_split(parent: &BTreeSet<Ref>, children: &[ChildSpec]) -> RoutingVerdict;
/// Merge: the union, always (§3). Ambiguous only in `model_override` / `tick_floor` conflicts,
/// which resolve from the SURVIVOR by rule, so a merge's routing verdict is total.
pub fn plan_merge(survivor: &AgentRow, absorbed: &AgentRow) -> RoutingPlan;
```

Step types (owner `graph-ops`), all `ClassRule::Evidence` — a structure change is a fact and it
cites what justified it:

| type | body |
|---|---|
| `graph/split` | `{ parent: TrajId, at_seq: Seq, children: Vec<ChildRecord>, reason, by }` |
| `graph/merge` | `{ survivor: AgentName, absorbed: AgentName, survivor_traj, absorbed_traj, at_seq, reconciliation: RollupId, reason, by }` |
| `graph/bud` | `{ parent: TrajId, child: TrajId, at_seq, agent: Option<AgentName>, routing_refs: Vec<Ref>, reason, by }` (a fork is a bud with `agent: None`) |
| `graph/undo` | `{ of: StepId, shape: UndoShape, trajs: Vec<TrajId>, by }` |

`ChildRecord { traj, agent: Option<AgentName>, routing_refs: Vec<Ref>, digest: Option<RollupId> }`.

Ordering inside `apply` (P5-D8), identical for split, bud and fork:

1. resolve `at_seq` (P5-D7) and refuse with `GraphError::OpenWake { wake }` if it cannot be resolved;
2. `plan()`; a non-empty `questions` ⇒ `ask_leader` and `GraphError::Ambiguous`;
3. `ledger.fork(parent → child)` per child: the ancestor edge and the `fork/end-seed` marker;
4. `rollups.rebuild_digest(DigestRequest { agent, traj: child, parents: vec![parent_traj], .. })`
   per child — the only LLM cost (§4);
5. `put_agent` for each child with a row, then re-`put_agent` the parent with its reduced refs;
6. append the cited `graph/*` step LAST, naming everything above. A crash before step 6 leaves a
   trajectory and an edge that nothing points at — inert, and the op is simply re-runnable.

Merge additionally: two `EdgeKind::Merge` edges into a NEW head trajectory, one reconciliation
digest (`DigestRequest { parents: [survivor_traj, absorbed_traj], reconcile: true }`), the
survivor's row rewritten with the union of `routing_refs` and its OWN `model_override` /
`tick_floor` / `wake_classes`, and `delete_agent(absorbed)` — the losing ROW goes, both
trajectories stay (§3).

Undo: `undo` reads the `graph/*` step being undone, then asks the ledger whether either child
trajectory has any step beyond its `fork/end-seed`. None ⇒ `UndoShape::Pointers`: delete the child
rows, restore the parent's refs from the op step, append `graph/undo`. Any ⇒ `UndoShape::Merge`:
run the merge path with the parent as survivor, which writes the reconciliation digest, reroutes,
and leaves the divergent heads behind by construction (no trajectory is ever deleted).

Config: `GraphConfig { max_children: usize, digest_on_fork: bool, question_on_ambiguity: bool }`.
`question_on_ambiguity` may be set `false` only in tests; the row's validator refuses `false` when
`mail` is bound to a live tree — no, plainly: it is not configurable at all in `bough-base` and the
field exists so the ambiguity tests can assert the refusal path directly. (P5-D9.)

Invariant: every `graph/split` step is preceded by exactly two ancestor edges and two
`fork/end-seed` markers naming its `at_seq`; every `graph/merge` step is preceded by exactly two
`merge` edges and one `reconciliation` rollup whose `src_trajs` are both parents; no `agents` row
named by a `graph/merge` as `absorbed` exists afterwards.

### 2.4 Claims — `plugins/claims/src/…`

```rust
pub struct Claims;
impl ServiceKey for Claims {
    type Value = ClaimsHandle;
    const NAME: &'static str = "claims";
}

#[derive(Clone)]
pub struct ClaimsHandle(pub Arc<ClaimsInner>);

/// What a claim is ABOUT. The ledger's `ClaimProposed.kind` is a free string; this is the parsed
/// form, and an unknown kind stays `Other` and is accept/rejectable but does nothing structural.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ClaimKind {
    /// Accepted ⇒ a pin (§3: "accepted requirements are pins").
    Requirement { supersedes: Vec<StepId> },
    /// Accepted ⇒ an `agents` row is born through `ctx.graph` (bud from the proposing lane).
    Lane { name: AgentName, from_seq: Option<Seq>, routing_refs: BTreeSet<Ref>, wake_classes: BTreeSet<String> },
    Split(SplitProposal),
    Merge(MergeProposal),
    Bud(BudProposal),
    Contradiction { between: Vec<StepId> },
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenClaim {
    pub claim: ClaimId,
    pub proposal: StepId,
    pub traj: TrajId,
    pub by: AgentName,
    pub kind: ClaimKind,
    pub title: String,
    pub body: String,
    pub at: DateTime<Utc>,
    pub cites: Vec<Cite>,
}

/// The decision. `Accept` and `Edit` are ANDREY'S ACTS: see `Actor` below.
#[derive(Clone, Debug)]
pub enum Decision {
    Accept,
    Edit { title: String, body: String },
    Reject { reason: String },
}

/// Who is deciding. §16: acceptance is Andrey's act, so the seam takes it as a parameter and
/// refuses anything else — including a call made while an agent's ambient initiator is set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Actor { Andrey }

impl ClaimsHandle {
    pub async fn open(&self, q: &ClaimQuery) -> Result<Vec<OpenClaim>, ClaimsError>;
    pub async fn get(&self, claim: &ClaimId) -> Result<Option<OpenClaim>, ClaimsError>;

    /// The only writer of `claim/accepted` and `claim/rejected`.
    pub async fn decide(&self, req: DecideRequest) -> Result<DecideOutcome, ClaimsError>;

    /// A PROPOSAL: appends `claim/proposed`. Agents and plugins may call it; nothing else.
    pub async fn propose(&self, req: ProposeRequest) -> Result<OpenClaim, ClaimsError>;

    /// Rejection rate over a window, for drift-watch (§8). PURE over the steps it is handed.
    pub fn rejection_rate(steps: &[Step]) -> Option<Rate>;
}

#[derive(Clone, Debug)]
pub struct DecideRequest {
    pub claim: ClaimId,
    pub decision: Decision,
    pub actor: Actor,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecideOutcome {
    pub claim: ClaimId,
    /// `claim/accepted { edited }` or `claim/rejected { reason }`.
    pub step: StepId,
    /// Set when the acceptance produced a pin (a Requirement).
    pub pin: Option<StepId>,
    /// Set when the acceptance produced structure (a Lane / Split / Merge / Bud).
    pub graph: Option<OpOutcome>,
    /// Set when the acceptance BORN a lane: the row and the resident that now holds it.
    pub born: Option<AgentName>,
}
```

`decide` is the one place the phase's ground truth lives:

- `Accept` on a `Requirement` ⇒ `claim/accepted { edited: false }` then `pin/set { title, text,
  supersedes }`. Re-accepting or editing a requirement supersedes its old pin (§3), which is the
  `supersedes` list, not an edit.
- `Edit` ⇒ the same, with `edited: true` and the EDITED text pinned. The proposal step is never
  rewritten; the edit is a new fact citing it.
- `Accept` on a `Lane` ⇒ `ctx.graph.apply(OpRequest::Bud(..))` with `agent: Some(name)`, then the
  agents row exists and `ctx.agents.resume(name)` brings the resident up in the same transaction:
  a claim accepted as a new lane births an `agents` row AND a live agent, or neither.
- `Accept` on `Split` / `Merge` / `Bud` ⇒ the corresponding `ctx.graph` op.
- `Reject` ⇒ `claim/rejected { reason }` and nothing else, ever.
- Any `decide` reached while `bough_plugin_agents::initiator::current()` is `Some` is refused with
  `ClaimsError::NotAndreysAct`. Ambient presence is never authorization (§2) — here it is the
  proof that the caller is a wake, and a wake may not accept.

Events: `claim/decided` — EMIT, payload `DecideOutcome` (the focus pane and drift-watch listen).

Tools registered by this row: the GLOBAL `propose_claim` (any lane agent may propose a
`Requirement`, `Contradiction` or `Other`; a structural kind from a lane agent is refused with the
reason "only the leader proposes structure", §2). Its leader-scoped twin lives in `tool-leader`
and accepts the structural kinds — that is V6's shadowing subject, and it is a real difference in
behaviour rather than a test fixture.

Commands: `/claims`, `/accept <claim>`, `/edit <claim> <text…>`, `/reject <claim> <reason…>`.

### 2.5 The leader set — `plugins/leader` + `plugins/tool-leader`

```rust
// plugins/leader/src/lib.rs
pub struct Leader;
impl ServiceKey for Leader {
    type Value = LeaderHandle;
    const NAME: &'static str = "leader";
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaderConfig {
    /// THE field this phase's SWAP test edits. The one agent whose scope holds the set.
    pub agent: String,
    /// The persona section's text, contributed at `Slot::Identity / Place::After`.
    pub persona: String,
    /// How many unsorted items `adopt_unsorted` may take at once.
    pub adopt_batch: usize,
    /// Attribute reconsolidation passes to the leader (§8) when `reconsolidation` is bound.
    pub attribute_reconsolidation: bool,
}

#[derive(Clone)]
pub struct LeaderHandle(pub Arc<LeaderInner>);

impl LeaderHandle {
    /// The agent this set is mounted for. `tool-leader` reads it; nothing else needs it.
    pub fn target(&self) -> &AgentName;
    /// Unsorted adoption (§2): read the queue, route each item to a lane, or hold it.
    pub async fn adopt(&self, req: AdoptRequest) -> Result<AdoptReport, LeaderError>;
    /// Requirement drafting from Andrey's words (§2): a claim, never a pin. Acceptance is his.
    pub async fn draft_requirement(&self, req: DraftRequest) -> Result<OpenClaim, LeaderError>;
    /// Cross-agent timeline DATA (§17: the surface is Phase 8).
    pub async fn note_timeline(&self, e: TimelineEntry) -> Result<StepId, LeaderError>;
    pub async fn timeline(&self, q: &TimelineQuery) -> Result<Vec<TimelineRow>, LeaderError>;
}
```

Step type (owner `leader`): `timeline/entry`, `ClassRule::Evidence`,
`{ title: String, at: String, agents: Vec<AgentName>, refs: Vec<Ref> }` — cited, because a
timeline is rendered as truth.

What the row registers, and where each registration LIVES vs. where it is VISIBLE (the distinction
that makes SWAP work):

| registration | owned by | visible to |
|---|---|---|
| the persona section | the `leader` row's fiber | `SectionScope::Agent(target)` |
| `unsorted_sink` on `ctx.mail` | the `leader` row's fiber | globally (it names the target) |
| `ctx.leader` | the `leader` row's fiber | the tree |
| the five tools | the `tool-leader` row's fiber | `ToolScope::Agent(target)` |

Every one of them is an effect. Editing `leader.config.agent` is a material config diff, so the
`leader` row reloads: its effects unwind (the section leaves the old agent's scope, the sink is
replaced by the null sink), `ctx.leader` is withdrawn, `tool-leader` — which injects `leader` —
unloads with it and reloads against the new binding, registering its tools for the new target.
No compile, no restart. `tool-leader` deliberately has NO `agent` field of its own: two rows with
two spellings of the same target is a misconfiguration waiting to happen (P5-D10).

`tool-leader`'s tools, all `ToolScope::Agent(leader)`:

| tool | shadows | does |
|---|---|---|
| `propose_claim` | the global one from `claims` | accepts the structural kinds the global one refuses |
| `adopt_unsorted` | — | reads `ctx.mail.unsorted()` and routes or holds |
| `draft_requirement` | — | `claim/proposed { kind: requirement }` from Andrey's words |
| `propose_structure` | — | `claim/proposed { kind: split \| merge \| bud }`, never an op |
| `note_timeline` | — | `timeline/entry` |

### 2.6 Per-lane scope — `plugins/lane-scope/src/lib.rs`

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneScopeConfig {
    /// The GLOBAL persona section, contributed under `SectionId::new("persona")`. It exists so a
    /// lane's scoped persona has a twin to shadow — without it "most-specific-wins" has nothing
    /// to win against and V6 could only assert that a section appeared. `None` ⇒ no global
    /// section, and a lane's persona is then simply additive.
    pub default_persona: Option<String>,
    /// One entry per lane that wants a scoped world. A name with no live agent is a WARNING at
    /// apply and a retry on `agent/created`, not a boot failure: lanes are born at runtime.
    pub lanes: Vec<LaneSpec>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneSpec {
    pub agent: String,
    /// Replaces the global persona section FOR THIS AGENT (same `SectionId`, agent scope).
    pub persona: Option<String>,
    /// §5's intersection filter. `allow: None` ⇒ everything the deny list admits.
    pub allow: Option<Vec<String>>,
    pub deny: Vec<String>,
}
```

It registers, per lane: a `SectionSpec { id: SectionId::new("persona"), scope: Agent, agent }` and
a `ToolsHandle::restrict(ctx, agent, Restrict { .. })`. Both from the ROW's ctx, so a patch that
drops a lane from the list unwinds exactly that lane's registrations. This row is why V6 has a
subject that is not the leader: shadowing and restriction must work for an ordinary lane.

### 2.7 `worker-fork` and the pinned prefix

§10: "child from the parent's history, one-shot, keeps the parent's request prefix". Byte-identity
of the prefix is the cache contract, and it cannot fall out of a child's own projection: the
child's identity band names the child, and its verbatim tail carries the `fork/end-seed` marker.
So the prefix is PINNED.

```rust
// plugins/projection/src/lib.rs — one method added to the Definition (WP-6)
#[async_trait::async_trait]
pub trait Projector: Send + Sync + 'static {
    // … existing: section, assemble, file_view, write_file_view …

    /// Pin an already-assembled prefix for ONE agent. `assemble` returns it verbatim for that
    /// agent, whatever the request's budget or `as_of` says, and records `source` so the request
    /// stays reconstructible from the ledger. Synchronous; the caller wraps it in an effect, so
    /// the pin unwinds with the agent that holds it.
    fn pin_prefix(&self, agent: AgentName, prefix: Assembled, source: PrefixSource)
        -> Result<PrefixToken, ProjectionError>;
}

/// Where a pinned prefix came from. Written durably as `fork/prefix`, so "the sent request
/// reconstructs from the ledger" (§0.2) survives pinning: re-assembling `of_agent` at `as_of`
/// reproduces the pin.
#[derive(Clone, Debug, PartialEq)]
pub struct PrefixSource { pub of_agent: AgentName, pub as_of: Seq }
```

```rust
// plugins/worker-fork/src/lib.rs
pub struct ForkProvider { /* … */ }

#[async_trait::async_trait]
impl WorkerProvider for ForkProvider {
    fn kinds(&self) -> Vec<WorkerKind> { vec![WorkerKind::Fork] }
    async fn start(&self, req: Arc<StartWorker>, run: WorkerRun) -> Result<WorkerResult, WorkerError>;
}

/// PURE: the seq a fork may branch at. The parent's head when it is outside an open wake, else
/// the last seq that is (P5-D7). Never pauses, never waits, never clips silently.
pub fn fork_point(steps_desc: &[Step]) -> Option<Seq>;
```

`start`, in order: `fork_point` → `ledger.fork(parent → worker-fork-<id>)` → assemble the PARENT
at that seq → `agents.create(CreateAgent { kind: AgentKind::Fork, traj: <the forked child>,
setup })` where `setup` (a) pins the prefix, (b) appends `fork/prefix { of_agent, as_of }`,
(c) registers the report tool and the step budget exactly as `worker-spawn` does. The seed message
is the task. The parent's message history reaches the child through `transcript::rebuild` over the
forked chain, which is the other half of "keeps the parent's history".

Step type (owner `worker-fork`): `fork/prefix`, `ClassRule::Thought`,
`{ of_agent: AgentName, as_of: Seq }`.

`tool-fork` (a third row in the existing `plugins/tool-workers` crate): name `fork`, args
`{ task: String, max_steps: Option<u32> }`, calls `ctx.workers.start(StartWorker { kind: Fork, .. })`.
The report lands as cited evidence in the spawner's chain through the existing `worker/report`
path — nothing new.

### 2.8 TUI

**The field bug (one step, one paragraph).** `rows_from_steps` coalesces CONSECUTIVE `thought/text`
steps sharing `(wake, step_index)` into ONE `Row::Text`, and consecutive `thought/reasoning` steps
the same way. The join is raw concatenation — the chunks are a split of one stream, and inserting
a separator is exactly what produced `"I'll run that" / " shell command for you."` on two lines.

```rust
pub enum Row {
    Text {
        /// The FIRST step of the group: the click/flash anchor, and what `Row::step()` returns.
        step: StepId,
        /// Every step folded into this row, oldest first. `parts.len() > 1` is the joined case.
        parts: Vec<StepId>,
        wake: WakeId,
        index: u32,
        text: String,
    },
    // … Reasoning gains `parts` on the same rule; the other variants are unchanged.
}
```

Consequences, all simplifications: `trailing_durable` becomes "the text of the trailing `Row::Text`"
and `trailing_text_rows` returns at most one index, so `lines()` loses the `position(|&g| g == i)`
dance and keeps only P3-D12's choice between the durable concatenation and the live tail. Streaming
is unchanged in behaviour and now flows through the same single row, which is what makes the
assertion hold "both while streaming and after the step lands".

**Claim cards.** `claim/proposed` renders as `Row::Claim { step, claim, kind, title, body, state }`
with `state` folded from later `claim/accepted` / `claim/rejected` steps in the same trajectory —
by step-type NAME (P3-D11), so `tui-focus` gains no dependency on `claims`. An open card draws
three hit regions, `claim:<id>:accept|edit|reject`; a click dispatches `ctx.claims.decide` with
`Actor::Andrey` (a click is Andrey's hand on the keyboard, and the seam refuses anything with an
ambient initiator). `edit` opens the composer pre-filled with the claim body and submits through
`/edit <claim> …`. The keyboard path is the three `claims` commands, which is what the shell-use
script drives.

**The branch picker.** `^b` (and a click on the trajectory header) opens a list of the focused
agent's branches: `ledger.edges(traj)` filtered to `EdgeKind::Ancestor` children, each row showing
the child traj, its `at_seq`, whether it has an `agents` row (a lane) or not (a fork), and its step
count. Enter switches the pane to that trajectory; `Esc` returns. Switching a fork into view is a
pane-local trajectory override, not a `FocusRequest`: a fork has no agent to focus.

**The strip.** `RailRow` gains `dormant: bool`, maintained from `ledger/step` of kind
`agent/dormancy` keyed by trajectory (the `about/line` precedent). `glyph()` gains one arm, ahead
of the status arms and behind `disposed`: `('◌', "dim")`, word `dormant`. Click-to-focus already
works (`rail::focus_for_hit`); Phase 3 deferred VERIFYING it because there was one agent. It is
verified here.

### 2.9 Step types added by this phase

| type | owner | class | why it is a step and not a side channel |
|---|---|---|---|
| `mail/unrouted` | mail-router | Evidence | the unsorted queue must survive restart |
| `mail/adopted` | mail-router | Evidence | adoption is attributable |
| `agent/routing` | mail-router | Evidence | a routing change explains later deliveries |
| `leader/question` | mail-router | Thought | a question is not truth |
| `agent/dormancy` | dormancy | Either | dormancy is derived, never stamped (P5-D2) |
| `graph/split` `graph/merge` `graph/bud` `graph/undo` | graph-ops | Evidence | §4's "cited split event" |
| `timeline/entry` | leader | Evidence | rendered as truth in Phase 8 |
| `fork/prefix` | worker-fork | Thought | the pinned prefix's reconstruction anchor |

Nothing new is needed for claims or pins: `claim/proposed`, `claim/accepted`, `claim/rejected`,
`pin/set` and `pin/retire` have existed since Phase 1.

### 2.10 Bundle rows

`bough-base` (headless included — routing, dormancy, claims and graph ops are not TUI features):

```yaml
- id: mail
  plugin: mail-router
  config: { unsorted_traj: "unsorted", unsorted_limit: 200, deliver_to_dormant: true }
- id: dormancy
  plugin: dormancy
  config: { reload_at_activation: true }
- id: graph
  plugin: graph-ops
  config: { max_children: 2, digest_on_fork: false, question_on_ambiguity: true }
- id: claims
  plugin: claims
  config: { open_limit: 50 }
- id: lane.scope
  plugin: lane-scope
  config: { default_persona: "…", lanes: [] }
- id: worker.fork
  plugin: worker-fork
  config: { max_steps: 24 }
- id: tool.fork
  plugin: tool-fork
```

`bough-tui-app` gains the leader GROUP (one entry, two children, so a patch can disable the whole
set with one `disabled: true`) and a wider `residents.bootstrap`:

```yaml
- id: leader.set
  group:
    - id: leader
      plugin: leader
      config: { agent: sol, persona: "…", adopt_batch: 20, attribute_reconsolidation: true }
    - id: tool.leader
      plugin: tool-leader
      config: {}
```

---

## Work packages

Eight packages, disjoint file sets. WP-1 and WP-2 are the only ones others depend on: WP-3..WP-7
program against the signatures above. WP-8 owns every shared file (`Cargo.toml`,
`crates/bough/Cargo.toml`, `bundles/*.yml`, `crates/bough/tests/`, `scripts/tui/`, `BUILD.md`), so
no two packages ever edit one file.

### WP-1: `mail-router` — the fan-out, the unsorted queue, late links

**Files:** `plugins/mail-router/` (`Cargo.toml`, `src/lib.rs`, `src/envelope.rs`, `src/matching.rs`,
`src/unsorted.rs`, `src/link.rs`, `src/question.rs`, `src/vocabulary.rs`, `src/error.rs`,
`src/invariant.rs`, `tests/fanout.rs`, `tests/unsorted.rs`, `tests/link.rs`).

The seam of 2.1. `route` seeds a `RouteDecision` from the pure matcher, dispatches the `mail/route`
waterfall, then calls `Agent::deliver` once per surviving recipient — never its own append path, so
P3-D15's ordering and per-agent consumption come for free. Zero recipients ⇒ one `mail/unrouted`
step on the unsorted trajectory plus, when a sink is mounted, one ordinary-class delivery to the
sink's agent. `link_ref` rewrites the row and appends `agent/routing`; it never queries for
history, which is how `backfilled: 0` is a fact rather than a promise. `ask_leader` appends
`leader/question` and routes it wake-class.

Unit tests it must ship: `matching::tests::{every_matching_agent_is_returned_not_the_best_one,
a_partial_ref_overlap_matches, an_agent_with_no_routing_refs_matches_nothing,
recipients_are_name_ordered_and_deduplicated, wake_classes_of_reads_the_class_namespace_only}`;
`link::tests::{link_adds_and_unlink_removes, linking_a_ref_twice_is_idempotent,
a_link_reports_zero_backfilled}`; `unsorted::tests::{a_zero_match_envelope_becomes_one_unrouted_step,
adoption_names_the_unrouted_step_it_consumes}`; `invariant::tests::{a_planted_unrouted_step_whose_refs_matched_a_row_is_reported,
a_delivery_with_two_steps_for_one_recipient_is_reported, a_clean_stream_passes}`;
`tests/fanout.rs::{one_event_reaches_every_matching_agent,
each_recipient_gets_its_own_mail_delivered_step_and_seq,
consumption_by_one_agent_leaves_the_others_unconsumed,
a_misroute_to_a_third_agent_does_not_strand_the_true_owner,
a_route_listener_may_add_a_recipient, a_route_listener_that_skips_next_short_circuits}`;
`tests/unsorted.rs::{zero_matches_lands_in_the_unsorted_queue,
the_leader_sink_receives_it_as_ordinary_mail,
with_no_sink_the_queue_keeps_it_and_a_later_sink_adopts_it,
ask_leader_is_wake_class_and_carries_class_ask}`;
`tests/link.rs::{a_late_linked_ref_queues_no_backlog,
a_late_linked_ref_exposes_history_through_connected,
delivery_after_the_link_starts_at_link_time}`.

All offline: `ledger-memory` + `agent-loop-scripted`.

### WP-2: `dormancy`, and the `agent/wake-request` admission point

**Files:** `plugins/dormancy/` (whole crate: `src/lib.rs`, `src/fold.rs`, `src/admit.rs`,
`src/command.rs`, `src/vocabulary.rs`, `src/invariant.rs`, `tests/dormant.rs`,
`tests/reactivate.rs`); `plugins/agents/src/{events.rs, agent.rs, factory.rs, lib.rs,
invariant.rs}`; `plugins/agent-loop/src/{driver.rs, mail.rs, invariant.rs}`;
`plugins/agent-loop-scripted/src/lib.rs`.

The waterfall, the three `WakeCause` variants, and the listener. The loop edits are the smallest
that can exist: one dispatch at `spawn_wake`'s head, one extra argument to
`standing_invariant_holds`, and the scripted driver's matching dispatch. `agents`' invariant gains
one clause: no `wake/start` step exists for an agent that was dormant at that seq per the fold —
which is the durable form of "no ticks and no drain wakes".

Unit tests it must ship: `admit::tests::{a_live_agent_admits_every_kind,
a_dormant_agent_defers_a_drain, a_dormant_agent_defers_a_tick_and_a_catch_up,
andrey_always_reactivates_whatever_the_classes_say,
a_configured_wake_class_ref_reactivates, an_unconfigured_class_defers,
wake_class_matching_is_an_intersection_not_a_prefix}`;
`fold::tests::{the_last_dormancy_step_wins, no_step_means_awake, the_fold_ignores_other_trajectories}`;
`invariant::tests::{a_wake_started_while_dormant_is_reported, a_clean_stream_passes}`;
`tests/dormant.rs::{a_dormant_agent_opens_no_wake_for_ordinary_mail,
ordinary_mail_is_delivered_and_stays_unconsumed,
the_standing_invariant_holds_over_a_dormant_agent_with_a_backlog,
a_dormant_agent_keeps_its_routing_refs_and_wake_classes,
request_wake_returns_nothing_for_a_dormant_agent}`;
`tests/reactivate.rs::{an_andrey_message_reactivates_and_gets_a_sol_answer_wake,
a_wake_class_item_reactivates, reactivation_arms_one_drain_wake,
the_backlog_drains_by_the_standing_invariant,
reactivation_appends_one_dormancy_step_citing_the_trigger,
sleeping_twice_is_idempotent}`;
`plugins/agent-loop/src/mail.rs::tests::the_standing_invariant_is_satisfied_by_dormancy`.

### WP-3: `graph-ops` — split, merge, bud, fork, undo

**Files:** `plugins/graph-ops/` (`Cargo.toml`, `src/lib.rs`, `src/plan.rs`, `src/route.rs`,
`src/split.rs`, `src/merge.rs`, `src/bud.rs`, `src/undo.rs`, `src/seq.rs`, `src/vocabulary.rs`,
`src/error.rs`, `src/invariant.rs`, `tests/split.rs`, `tests/bud.rs`, `tests/merge.rs`,
`tests/undo.rs`, `tests/routing.rs`); plus the ONE additive edit that lets a merge produce a
reconciliation digest through the Phase 4 seam: `plugins/rollups/src/request.rs`
(`DigestRequest.reconcile`), `plugins/rollups/src/conformance.rs` (one case),
`plugins/rollups-summarizer/src/digest.rs` (the `recon:` namespace and `RollupKind::Reconciliation`),
`plugins/rollups-none/src/lib.rs` (the field, still sealing nothing).

The ops of 2.3 in the order 2.3 fixes, the pure planner, and the refusals. Nothing here writes a
rollup itself; every digest goes through `ctx.rollups`.

Unit tests it must ship: `route::tests::{a_ref_claimed_by_one_child_is_assigned,
a_ref_claimed_by_two_children_is_ambiguous, a_parent_ref_claimed_by_nobody_stays_with_the_parent,
merge_unions_the_refs, merge_takes_overrides_from_the_survivor,
the_planner_never_breaks_a_tie_by_name_or_order}`;
`seq::tests::{the_head_is_the_fork_point_when_no_wake_is_open,
an_open_trailing_wake_moves_the_point_below_it, a_bud_point_in_the_past_is_taken_as_given}`;
`plan::tests::{a_plan_is_total_every_child_is_planned_or_questioned,
a_plan_with_questions_names_every_ambiguous_ref}`;
`tests/split.rs::{a_split_writes_two_ancestor_edges_and_two_end_seeds,
one_inheritance_digest_per_child_naming_src_trajs, routing_refs_are_reassigned_and_the_parent_keeps_the_rest,
the_cited_split_step_is_appended_last_and_names_everything,
the_past_is_not_partitioned_both_children_still_read_it}`;
`tests/bud.rs::{a_bud_from_a_past_seq_leaves_the_parent_chain_whole,
the_parents_running_wake_completes_untouched, the_child_digest_names_src_trajs,
a_bud_with_no_agent_is_a_fork_with_no_row_and_no_routing,
promoting_a_fork_is_adding_the_row_and_nothing_else}`;
`tests/merge.rs::{one_new_head_and_two_merge_edges,
one_reconciliation_digest_spanning_both_parents, routing_refs_are_unioned,
overrides_come_from_the_survivor, the_losing_row_is_deleted,
both_trajectories_still_read_after_the_merge, every_sealed_tier_stays_valid,
a_merge_with_no_survivor_named_is_a_leader_question}`;
`tests/undo.rs::{undoing_an_unused_split_is_pointers_only,
an_unused_undo_writes_no_digest_and_calls_no_model, undoing_a_lived_in_split_is_a_merge,
divergent_heads_are_left_behind_and_named_in_the_undo_step}`;
`tests/routing.rs::{an_ambiguous_split_produces_a_leader_question,
no_split_is_written_while_the_question_is_open, the_question_is_wake_class_mail}`;
`invariant::tests::{a_split_without_two_edges_is_reported,
a_merge_whose_absorbed_row_still_exists_is_reported, a_clean_stream_passes}`.

### WP-4: `claims` — propose, accept, edit, reject, and the drift signal

**Files:** `plugins/claims/` (`Cargo.toml`, `src/lib.rs`, `src/kind.rs`, `src/decide.rs`,
`src/pin.rs`, `src/query.rs`, `src/tool.rs`, `src/command.rs`, `src/rate.rs`, `src/error.rs`,
`src/invariant.rs`, `tests/accept.rs`, `tests/reject.rs`, `tests/lane.rs`);
`plugins/drift-watch/src/signals.rs` + `plugins/drift-watch/tests/signals.rs` (the activation).

The seam of 2.4. `decide` is the only writer of `claim/accepted` / `claim/rejected`, the only
caller of `pin/set` for a requirement, and the only path from a claim to `ctx.graph`. The
`Actor::Andrey` refusal is asserted, not documented. drift-watch's `claim_rejection` stops
returning `SignalState::Inactive` and computes a rate from `claim/proposed` vs `claim/rejected` in
the window, still `Inactive` when the window holds no decided claim (a rate over zero decisions is
not a number).

Unit tests it must ship: `kind::tests::{a_known_kind_parses, an_unknown_kind_is_other_and_harmless,
a_structural_kind_from_a_lane_agent_is_refused}`;
`rate::tests::{the_rate_is_rejected_over_decided, an_undecided_window_is_inactive,
edits_count_as_acceptances}`;
`tests/accept.rs::{accepting_a_requirement_appends_a_pin,
the_pin_supersedes_the_requirements_previous_pin,
an_edit_accepts_with_edited_true_and_pins_the_edited_text,
the_proposal_step_is_never_rewritten, an_accept_from_a_wake_is_refused}`;
`tests/reject.rs::{a_rejection_records_a_reason, a_rejection_births_nothing,
a_rejected_claim_leaves_the_open_list}`;
`tests/lane.rs::{accepting_a_lane_claim_births_an_agents_row,
the_new_lane_is_a_bud_of_the_proposing_trajectory,
the_new_lane_carries_the_routing_refs_from_the_claim,
a_failed_birth_leaves_no_row_and_no_acceptance}`;
`plugins/drift-watch/src/signals.rs::tests::{claim_rejection_is_a_rate_once_claims_are_decided,
claim_rejection_stays_inactive_with_no_decided_claim}`.

### WP-5: the leader set and per-lane scope

**Files:** `plugins/leader/` (`Cargo.toml`, `src/lib.rs`, `src/adopt.rs`, `src/draft.rs`,
`src/timeline.rs`, `src/persona.rs`, `src/vocabulary.rs`, `src/error.rs`, `src/invariant.rs`,
`tests/scope.rs`, `tests/adopt.rs`); `plugins/tool-leader/` (`Cargo.toml`, `src/lib.rs`,
`src/tools.rs`, `src/invariant.rs`, `tests/tools.rs`); `plugins/lane-scope/` (`Cargo.toml`,
`src/lib.rs`, `src/invariant.rs`, `tests/scope.rs`).

The set of 2.5 and the lane scope of 2.6. Every registration is an effect owned by its row and
scoped to the target agent by SPEC, which is the whole mechanism behind SWAP; `tool-leader` reads
its target from `ctx.leader` and never from its own config.

Unit and integration tests they must ship: `leader` `tests/scope.rs::{the_persona_section_is_visible_to_the_target_only,
unloading_the_row_removes_the_section, moving_the_target_moves_the_section,
the_unsorted_sink_names_the_target, unloading_the_row_restores_the_null_sink}`;
`leader` `tests/adopt.rs::{adopt_routes_an_unsorted_item_to_a_lane,
adopt_appends_mail_adopted_naming_the_unrouted_step, adopt_holds_what_it_cannot_place,
draft_requirement_produces_a_claim_and_never_a_pin,
note_timeline_appends_a_cited_entry}`;
`tool-leader` `tests/tools.rs::{the_five_tools_are_in_the_targets_schema,
they_are_absent_from_every_other_agents_schema,
the_executor_refuses_them_for_another_agent,
the_scoped_propose_claim_accepts_a_structural_kind,
the_global_one_refuses_it_for_a_lane_agent,
the_row_reloads_when_the_leader_binding_changes}`;
`lane-scope` `tests/scope.rs::{a_scoped_persona_replaces_the_global_section_for_that_agent,
another_agent_still_sees_the_global_section, restrict_is_an_intersection_of_two_restrictions,
a_filtered_tool_is_absent_from_the_schema, a_filtered_tool_is_refused_by_the_executor,
a_workers_scope_inherits_nothing_from_its_spawner,
a_lane_named_by_config_that_does_not_exist_yet_is_a_warning_then_a_retry}`.

### WP-6: `worker-fork`, `tool-fork`, and the pinned prefix

**Files:** `plugins/worker-fork/` (`Cargo.toml`, `src/lib.rs`, `src/point.rs`, `src/prefix.rs`,
`src/vocabulary.rs`, `src/invariant.rs`, `tests/prefix.rs`, `tests/oneshot.rs`);
`plugins/projection/src/{lib.rs, section.rs}` (the `pin_prefix` method + `PrefixSource`);
`plugins/projection-assembler/src/{lib.rs, pin.rs}` + `plugins/projection-assembler/tests/pin.rs`
(the sole implementor); `plugins/tool-workers/src/lib.rs` + `tests/fork_tool.rs`
(the `tool-fork` row).

The provider of 2.7. The pin is an effect held by the child agent's setup, so it unwinds with the
child and nothing global remembers it. `fork/prefix` is what keeps §0.2's reconstruction rule
true through a pin.

Unit tests they must ship: `point::tests::{the_head_is_the_point_when_no_wake_is_open,
an_open_trailing_wake_moves_the_point_below_it, an_empty_chain_has_no_point}`;
`prefix::tests::{a_pin_is_returned_verbatim_whatever_the_budget,
a_pin_for_one_agent_does_not_leak_to_another, disposing_the_token_restores_normal_assembly}`;
`projection-assembler` `tests/pin.rs::{a_pinned_prefix_is_byte_identical_to_what_was_pinned,
an_unpinned_agent_assembles_normally}`;
`worker-fork` `tests/prefix.rs::{the_childs_system_prefix_equals_the_parents_at_the_fork_seq,
the_request_header_digest_matches_the_parents,
the_fork_prefix_step_names_the_parent_and_the_seq,
re_assembling_the_parent_at_that_seq_reproduces_the_pin}`;
`worker-fork` `tests/oneshot.rs::{the_child_sees_the_parents_message_history,
the_report_lands_as_cited_evidence_in_the_spawner_chain,
the_child_is_disposed_after_one_report, a_fork_inside_an_open_wake_branches_below_it,
the_fork_bound_counts_against_the_same_spawn_bounds}`;
`tool-workers` `tests/fork_tool.rs::{fork_starts_a_fork_kind_worker,
fork_is_refused_when_no_fork_provider_is_mounted}`.

### WP-7: the focus pane and the strip

**Files:** `plugins/tui-focus/src/{rows.rs, lib.rs, claims.rs, branches.rs}`,
`plugins/tui-focus/tests/{rows.rs, render.rs, stream.rs, claims.rs, branches.rs}`;
`plugins/tui-strip/src/{rail.rs, lib.rs}`, `plugins/tui-strip/tests/rail.rs`.

The three surface changes of 2.8: the paragraph join (the field bug), claim cards, the branch
picker; plus the strip's dormant glyph. All of it is pure-function-plus-listener work; the panes
gain no new injected key beyond `claims` (optional: a pane with no claims seam renders cards
read-only rather than disappearing).

Unit tests they must ship: `rows::tests::{two_text_steps_of_one_step_index_join_into_one_row,
the_join_is_raw_concatenation_with_no_separator,
a_tool_call_between_two_texts_breaks_the_group,
a_new_step_index_breaks_the_group, a_new_wake_breaks_the_group,
the_joined_row_anchors_on_the_first_step_and_lists_every_part,
two_reasoning_steps_join_on_the_same_rule,
a_claim_proposed_step_renders_as_a_card, an_accepted_claim_card_shows_its_state,
a_rejected_claim_card_shows_its_reason}`;
`render::tests::{a_joined_row_wraps_as_one_paragraph_at_width,
a_joined_row_draws_exactly_once, an_open_claim_card_draws_three_hit_regions}`;
`stream::tests::{the_live_tail_replaces_the_joined_durable_text_while_streaming,
the_landed_text_equals_the_streamed_text}`;
`branches::tests::{ancestor_children_are_listed_oldest_first,
a_child_with_an_agents_row_is_labelled_a_lane_and_one_without_a_fork,
selecting_a_branch_switches_the_panes_trajectory, esc_returns_to_the_agents_own_chain,
an_agent_with_no_children_renders_an_empty_picker}`;
`tui-strip` `rail::tests::{a_dormant_row_draws_the_dormant_glyph_and_word,
disposed_still_wins_over_dormant, dormancy_is_read_from_the_step_by_name,
focus_for_hit_maps_each_of_three_rails_to_its_own_agent}`.

### WP-8: integration — the rows, the wiring, the swap, the shell-use suite

**Files:** `plugins/residents/src/lib.rs` (+ `tests/catchup.rs`), `Cargo.toml`,
`crates/bough/Cargo.toml`, `bundles/bough-base.yml`, `bundles/bough-tui-app.yml`,
`crates/bough/tests/{leader_swap.rs, many_agents.rs, dormancy_loops.rs, graph_invariants.rs}`,
`scripts/tui/{12-many-agents.sh, 13-claims.sh, 14-forks.sh, 15-leader-swap.sh}`,
`scripts/tui/fixtures/{many-agents.patch.yml, leader-elsewhere.patch.yml, seed-lanes.sql}`,
`BUILD.md`, this file's status lines.

Multi-lane `residents` (bootstrap a list; a dormant row is resumed and simply never woken), the
seven `bough-base` rows and the `leader.set` group, the workspace and launcher wiring, and every
screen-level bullet. §17's testing policy: every TUI-visible behaviour of this phase gets a
shell-use script under `scripts/tui/` run by `make tui-test`.

Tests it must ship: `crates/bough/tests/many_agents.rs::{three_lanes_boot_and_appear_in_the_registry,
mail_fans_out_across_lanes_in_a_booted_tree, a_dormant_lane_runs_no_wake_over_a_whole_boot,
every_phase_five_invariant_runs_at_quiesce}`;
`crates/bough/tests/dormancy_loops.rs::{admission_is_dispatched_by_agent_loop,
admission_is_dispatched_by_agent_loop_scripted, a_deferred_wake_appends_no_wake_start_under_either}`;
`crates/bough/tests/graph_invariants.rs::{a_bud_in_a_booted_tree_leaves_the_parent_running,
the_ledger_and_agents_invariants_are_clean_after_a_split_and_a_merge}`;
`crates/bough/tests/leader_swap.rs::{the_leader_set_activates_in_one_agents_scope,
a_patch_moves_it_to_another_agent_without_a_recompile,
the_old_agent_loses_the_tools_from_its_schema, the_old_agent_is_refused_by_the_executor,
the_old_agent_loses_the_persona_section, the_new_agent_gains_all_three,
the_unsorted_sink_moved_with_it, nothing_in_the_tree_is_failed_after_the_move,
moving_it_back_restores_the_first_agent}`;
shell-use bullets named in the verification map below.

---

## 3. Verification map

Each brief bullet V1..V8 + SWAP, and §17 Phase 5's own three, against the test that proves it. A
bullet is DONE only when the named test has run green.

### V1 — bud a real agent from existing content mid-history

| claim | test |
|---|---|
| the past is not partitioned | `bough-plugin-graph-ops` `tests/bud.rs::a_bud_from_a_past_seq_leaves_the_parent_chain_whole` + `tests/split.rs::the_past_is_not_partitioned_both_children_still_read_it` (the parent's steps are unchanged and `connected(child)` reaches them) |
| the child's inheritance digest names `src_trajs` | `tests/bud.rs::the_child_digest_names_src_trajs` (a `RollupKind::Digest` row in the `digest:…:inherited` namespace whose `src_trajs == [parent_traj]`) |
| routing refs are reassigned | `tests/split.rs::routing_refs_are_reassigned_and_the_parent_keeps_the_rest`; pure: `route::tests::a_ref_claimed_by_one_child_is_assigned` |
| the parent never pauses | `tests/bud.rs::the_parents_running_wake_completes_untouched` (a wake is opened on the parent, the bud runs concurrently, the wake reaches `wake/end { reason: completed }` and its consumed set is intact) |
| in a booted tree, not just a fixture | `crates/bough/tests/graph_invariants.rs::a_bud_in_a_booted_tree_leaves_the_parent_running` |
| the fork point is resolved, never clipped | `seq::tests::an_open_trailing_wake_moves_the_point_below_it` |

### V2 — a claim accepted in the TUI births a lane

| claim | test |
|---|---|
| accept births an `agents` row | `bough-plugin-claims` `tests/lane.rs::accepting_a_lane_claim_births_an_agents_row` |
| the birth is a bud of the proposing chain | `tests/lane.rs::the_new_lane_is_a_bud_of_the_proposing_trajectory` |
| a requirement acceptance appends a pin | `tests/accept.rs::accepting_a_requirement_appends_a_pin`, `…::the_pin_supersedes_the_requirements_previous_pin` |
| the edit path | `tests/accept.rs::an_edit_accepts_with_edited_true_and_pins_the_edited_text` |
| the reject path | `tests/reject.rs::{a_rejection_records_a_reason, a_rejection_births_nothing}` |
| acceptance is Andrey's act | `tests/accept.rs::an_accept_from_a_wake_is_refused` |
| ON SCREEN: a driven accept births a lane and the strip shows it | `scripts/tui/13-claims.sh` bullets `{a_claim_card_renders_with_three_actions, accept_appends_claim_accepted, an_accepted_requirement_appears_as_a_pin, accepting_a_lane_claim_adds_a_rail_row, the_new_lane_has_an_agents_row, edit_pins_the_edited_text, reject_records_the_reason_and_births_nothing, a_click_on_accept_decides_the_card}` |

### V3 — ambiguous mail becomes a leader question

| claim | test |
|---|---|
| zero matches → the leader's unsorted queue | `mail-router` `tests/unsorted.rs::{zero_matches_lands_in_the_unsorted_queue, the_leader_sink_receives_it_as_ordinary_mail}` |
| the queue survives having no leader | `…unsorted.rs::with_no_sink_the_queue_keeps_it_and_a_later_sink_adopts_it` |
| a routing conflict is a question, never a guess | `graph-ops` `tests/routing.rs::an_ambiguous_split_produces_a_leader_question`; pure: `route::tests::{a_ref_claimed_by_two_children_is_ambiguous, the_planner_never_breaks_a_tie_by_name_or_order}` |
| nothing is written while the question is open | `tests/routing.rs::no_split_is_written_while_the_question_is_open` |
| a merge with no survivor named is a question too | `tests/merge.rs::a_merge_with_no_survivor_named_is_a_leader_question` |
| the question reaches the leader as wake-class mail | `tests/routing.rs::the_question_is_wake_class_mail`; `mail-router` `tests/unsorted.rs::ask_leader_is_wake_class_and_carries_class_ask` |

### V4 — dormancy

| claim | test |
|---|---|
| no ticks, no drain wakes | `dormancy` `tests/dormant.rs::{a_dormant_agent_opens_no_wake_for_ordinary_mail, request_wake_returns_nothing_for_a_dormant_agent}`; pure: `admit::tests::{a_dormant_agent_defers_a_drain, a_dormant_agent_defers_a_tick_and_a_catch_up}` |
| under BOTH loop providers | `crates/bough/tests/dormancy_loops.rs::{admission_is_dispatched_by_agent_loop, admission_is_dispatched_by_agent_loop_scripted, a_deferred_wake_appends_no_wake_start_under_either}` |
| ordinary mail queues silently | `tests/dormant.rs::ordinary_mail_is_delivered_and_stays_unconsumed` |
| keep and routing are kept | `tests/dormant.rs::a_dormant_agent_keeps_its_routing_refs_and_wake_classes` |
| an Andrey message reactivates | `tests/reactivate.rs::an_andrey_message_reactivates_and_gets_a_sol_answer_wake`; pure: `admit::tests::andrey_always_reactivates_whatever_the_classes_say` |
| a configured wake class reactivates, an unconfigured one does not | `tests/reactivate.rs::a_wake_class_item_reactivates`; `admit::tests::{a_configured_wake_class_ref_reactivates, an_unconfigured_class_defers}` |
| the backlog drains by the standing invariant | `tests/reactivate.rs::{reactivation_arms_one_drain_wake, the_backlog_drains_by_the_standing_invariant}`; `agent-loop` `mail::tests::the_standing_invariant_is_satisfied_by_dormancy` |
| the durable trail, and the invariant | `dormancy` `invariant::tests::a_wake_started_while_dormant_is_reported`; `tests/reactivate.rs::reactivation_appends_one_dormancy_step_citing_the_trigger` |
| ON SCREEN | `scripts/tui/12-many-agents.sh` bullets `{a_dormant_lane_shows_the_dormant_glyph, a_dormant_lane_runs_no_wake_while_mail_arrives, waking_it_drains_the_backlog_in_one_wake}` |

### V5 — mail fan-out

| claim | test |
|---|---|
| every matching agent, not the best one | `mail-router` `tests/fanout.rs::one_event_reaches_every_matching_agent`; pure: `matching::tests::every_matching_agent_is_returned_not_the_best_one` |
| per-agent consumption | `tests/fanout.rs::{each_recipient_gets_its_own_mail_delivered_step_and_seq, consumption_by_one_agent_leaves_the_others_unconsumed}` |
| a misroute strands nobody | `tests/fanout.rs::a_misroute_to_a_third_agent_does_not_strand_the_true_owner` |
| a late-linked ref queues no backlog | `tests/link.rs::a_late_linked_ref_queues_no_backlog`; `link::tests::a_link_reports_zero_backfilled` |
| …but exposes history through `connected()` | `tests/link.rs::a_late_linked_ref_exposes_history_through_connected` |
| delivery starts at link time | `tests/link.rs::delivery_after_the_link_starts_at_link_time` |
| in a booted tree | `crates/bough/tests/many_agents.rs::mail_fans_out_across_lanes_in_a_booted_tree` |

### V6 — per-agent scope shadowing

| claim | test |
|---|---|
| a scoped section replaces its global twin, for that agent only | `lane-scope` `tests/scope.rs::{a_scoped_persona_replaces_the_global_section_for_that_agent, another_agent_still_sees_the_global_section}` |
| a scoped tool shadows its global twin | `tool-leader` `tests/tools.rs::{the_scoped_propose_claim_accepts_a_structural_kind, the_global_one_refuses_it_for_a_lane_agent}` (Phase 2's `plugins/tools/tests/scope.rs` already pins the registry rule; this is its behavioural twin) |
| scoped registrations never inherit to workers | `lane-scope` `tests/scope.rs::a_workers_scope_inherits_nothing_from_its_spawner` |
| `restrict` is an intersection | `lane-scope` `tests/scope.rs::restrict_is_an_intersection_of_two_restrictions`; pure: `plugins/tools/src/registry.rs::tests::intersect_narrows_the_allow_list_and_unions_the_denies` (Phase 2, still green) |
| a filtered tool is ABSENT and REFUSED | `lane-scope` `tests/scope.rs::{a_filtered_tool_is_absent_from_the_schema, a_filtered_tool_is_refused_by_the_executor}` |

### V7 — merge and undo

| claim | test |
|---|---|
| one surviving row, the loser deleted | `graph-ops` `tests/merge.rs::{one_new_head_and_two_merge_edges, the_losing_row_is_deleted}` |
| `routing_refs` unioned, overrides from the survivor | `tests/merge.rs::{routing_refs_are_unioned, overrides_come_from_the_survivor}`; pure: `route::tests::{merge_unions_the_refs, merge_takes_overrides_from_the_survivor}` |
| both trajectories and sealed tiers remain valid | `tests/merge.rs::{both_trajectories_still_read_after_the_merge, every_sealed_tier_stays_valid}` |
| one reconciliation digest spanning two parents | `tests/merge.rs::one_reconciliation_digest_spanning_both_parents` (a `RollupKind::Reconciliation` row whose `src_trajs` are both parents) |
| undoing an unused split is pointers only | `tests/undo.rs::{undoing_an_unused_split_is_pointers_only, an_unused_undo_writes_no_digest_and_calls_no_model}` |
| undoing a lived-in one is a merge | `tests/undo.rs::{undoing_a_lived_in_split_is_a_merge, divergent_heads_are_left_behind_and_named_in_the_undo_step}` |
| the invariants hold in a booted tree | `crates/bough/tests/graph_invariants.rs::the_ledger_and_agents_invariants_are_clean_after_a_split_and_a_merge` |

### V8 — worker-fork, click-to-focus, the branch picker, one paragraph

| claim | test |
|---|---|
| the parent's request prefix is byte-identical | `worker-fork` `tests/prefix.rs::{the_childs_system_prefix_equals_the_parents_at_the_fork_seq, the_request_header_digest_matches_the_parents}` |
| …and still reconstructible from the ledger | `tests/prefix.rs::{the_fork_prefix_step_names_the_parent_and_the_seq, re_assembling_the_parent_at_that_seq_reproduces_the_pin}` |
| the result lands as cited evidence | `worker-fork` `tests/oneshot.rs::the_report_lands_as_cited_evidence_in_the_spawner_chain` |
| ON SCREEN: click-to-focus between two agents | `scripts/tui/12-many-agents.sh` bullets `{three_rails_render_with_their_glyphs, a_click_on_the_second_rail_focuses_it, the_focus_pane_follows_the_click, a_click_back_returns_to_the_first}` (the Phase 3 deferred check) |
| ON SCREEN: the branch picker lists forks and switches | `scripts/tui/14-forks.sh` bullets `{the_picker_lists_the_agents_branches, a_lane_child_and_a_fork_child_are_labelled_differently, selecting_a_branch_shows_its_trajectory, esc_returns_to_the_agents_own_chain}` |
| ON SCREEN: a multi-chunk answer is ONE wrapped paragraph | `scripts/tui/12-many-agents.sh` bullets `{a_two_chunk_answer_renders_as_one_paragraph_while_streaming, and_still_one_paragraph_after_the_step_lands, the_ledger_still_holds_two_thought_text_steps}` — the third bullet is what keeps the fix a RENDER join rather than a quietly merged ledger |
| the pure half of the same fix | `tui-focus` `rows::tests::{two_text_steps_of_one_step_index_join_into_one_row, the_join_is_raw_concatenation_with_no_separator, a_tool_call_between_two_texts_breaks_the_group}` and `render::tests::a_joined_row_wraps_as_one_paragraph_at_width` |

### SWAP — move the `leader` set to another agent by patch

| claim | test |
|---|---|
| it activates in one agent's scope | `crates/bough/tests/leader_swap.rs::the_leader_set_activates_in_one_agents_scope` |
| a patch moves it, no compile, no restart | `…::a_patch_moves_it_to_another_agent_without_a_recompile` (through the launcher's own live recompose, the `loop_swap.rs` / `rollups_swap.rs` precedent) |
| the old agent loses the tools from its SCHEMA | `…::the_old_agent_loses_the_tools_from_its_schema` |
| …and is REFUSED by the executor | `…::the_old_agent_is_refused_by_the_executor` (indistinguishable from a nonexistent tool: same `ToolsError` variant as an unregistered name) |
| the old agent loses the persona section | `…::the_old_agent_loses_the_persona_section` |
| the new agent gains all three | `…::the_new_agent_gains_all_three` |
| the unsorted sink moved with it | `…::the_unsorted_sink_moved_with_it` |
| the tree stays consistent | `…::{nothing_in_the_tree_is_failed_after_the_move, moving_it_back_restores_the_first_agent}` |
| ON SCREEN | `scripts/tui/15-leader-swap.sh` bullets `{the_leader_tools_are_offered_to_the_first_lane, the_patch_lands_without_a_restart, the_first_lane_no_longer_offers_them, the_second_lane_does}` |

### The phase's own gates

`make gates` green (build + lint + test + the replay half of the shell-use suite) and
`make tui-test` green in both halves, three consecutive clean runs, as Phases 3 and 4 required.
The live half runs `claude-haiku-4-5-20251001` for both sol and terra with the key from
`~/.bough/env`; the bullets that need a corpus or a timing window SKIP with a printed reason
rather than passing vacuously.

---

## 4. What Phase 5 does NOT build

Named because a reviewer will look for them: collectors and the `mcp` seam, the `actions`
Providers, `wards-rhai`, `hooks-exec`, `mcp-subprocess`, `skills`, `schedule-cron`, the
`sleep-listener`, idle ticks, the timeline SURFACE (the leader curates its DATA here; the pane is
Phase 8), the projection preview pane and the drift dashboard. A parallel track on `rebuild-b` is
adding collectors, mcp, actions providers and runtime hosts as NEW crates; nothing in this plan
edits a file that track owns.

---

## 5. Decisions taken where REQUIREMENTS is silent

- **P5-D1 — wake admission is a new waterfall, `agent/wake-request`, and it is the only loop
  change of the phase.** §1 says a dormant agent gets no ticks and no wakes; §5's flow starts at
  `wake/start`, and `agent/pre-step` fires INSIDE an already-durable wake. Suppressing a wake from
  `pre-step` would leave a trail of empty durable wakes for an agent that is supposed to cost
  nothing. The waterfall sits immediately before `wake/start`, both loop Providers dispatch it, and
  the default with no listener is `Open` — so nothing changes for a tree without the `dormancy`
  row. This amends §5's wake flow and is flagged as such per AGENTS.md ("changing the agent loop
  itself requires updating this document").
- **P5-D2 — dormancy is a DERIVED fold over `agent/dormancy` steps, not a column on `agents`.**
  §3 makes `agents` mutable config and would have tolerated a column, but it also says membership
  is derived and never stamped, and a column means a schema change in two ledger Providers plus the
  conformance suite for one boolean. The fold is one indexed query per agent at activation and a
  cached bool afterwards — the P2-D8 shape.
- **P5-D3 — a wake CLASS is a ref in the `class:` namespace.** §5 names classes ("asks, mentions of
  Andrey, review requests") and §3 makes `step_refs` canonical for matching and routing. Spelling a
  class as `class:ask` means `agents.wake_classes` matches through the same index the router
  already uses, and no ledger vocabulary type grows a field. `MailClass` keeps its two urgencies
  and gains no third meaning.
- **P5-D4 — the unsorted queue is a real trajectory, and the leader is a SINK on it, not its
  owner.** §3 says zero matches route to "the leader's unsorted queue". A tree may boot with no
  leader (headless, and the moment before the leader row activates), and mail must not be dropped
  or refused then. So the queue is durable and leaderless; the leader row installs a sink that also
  delivers new unsorted items live, and adopts the backlog when it arrives.
- **P5-D5 — `mail/route` is a waterfall, and the crate's own matcher is not a listener.** The
  matcher SEEDS the decision before dispatch, so a policy listener that deliberately skips `next()`
  short-circuits to a decision that already exists rather than to an empty one. A listener chain
  whose first link is the only source of truth is a chain where one buggy plugin silently drops
  every event.
- **P5-D6 — one crate for `mail-router` and one for `dormancy`, each providing its own key.** §0.2
  forbids preemptive splitting; each has one conceivable Provider and its Consumers are model-facing
  tools and the loop. The `commands` / `drift-watch` precedent (P4-D1's other half).
- **P5-D7 — a fork/split/bud point is RESOLVED, never clipped and never waited on.** §3 refuses a
  fork whose prefix ends inside an open wake. Rather than failing or pausing the parent (§4: "the
  parent never pauses"), `fork_point` walks down to the last seq outside an open wake and the op
  reports the seq it used. An explicit `at_seq` inside an open wake is an ERROR, not a silent
  adjustment: the caller named a point and deserves to know it was not legal.
- **P5-D8 — the cited `graph/*` step is appended LAST.** A crash mid-op leaves an orphan trajectory
  and an edge that nothing names — inert, invisible to `connected()` for any agent without a row,
  and the op is re-runnable. Appending the op step first would leave a cited fact naming
  trajectories that do not exist, which is the failure mode §16 cares about.
- **P5-D9 — `question_on_ambiguity` exists so the refusal path can be tested directly, and
  `bough-base` never sets it `false`.** Stated openly rather than hidden: it is a test seam, and
  the row's documentation says so. If a reviewer prefers no seam at all, the test can drive the
  ambiguity through `plan()` instead and the field goes.
- **P5-D10 — `tool-leader` has no `agent` field; it reads `ctx.leader.target()`.** Two rows with
  two spellings of one target is a misconfiguration that would present as "half the leader set
  moved". Injecting the key makes the move atomic: `leader` reloads, its binding withdraws,
  `tool-leader` unloads and reloads against the new one.
- **P5-D11 — the leader set's registrations are OWNED by their rows and SCOPED by spec.** Phase 2's
  `ToolScope::Agent(name)` and `SectionScope::Agent` already carry visibility; lifetime comes from
  the ctx the effect is registered on. Registering through `agent.ctx()` (the `worker-spawn`
  precedent) would tie the leader's tools to the AGENT's lifetime, and then moving the set would
  depend on the old agent being torn down. This is the difference that makes SWAP a config edit.
- **P5-D12 — the fork worker's prefix is PINNED through a new `Projector::pin_prefix`, and the pin
  is ledgered as `fork/prefix`.** §10 asks for the parent's request prefix byte-identical. A child's
  own projection cannot be that: the identity band names the child and the tail carries the
  `fork/end-seed` marker. The alternatives lost — assembling as the parent by aliasing the agent
  name would make two agents indistinguishable to every other consumer, and driving a private
  one-shot loop inside `worker-fork` would put loop code in a second crate (§2: `agent-loop` is
  the only one). The pin is an effect on the child's setup and a durable step, so §0.2's
  reconstruction rule survives it.
- **P5-D13 — `DigestRequest` grows ONE additive field, `reconcile: bool`, rather than a fourth
  rollup kind or a second seam.** §3 fixes three `RollupKind`s and names reconciliation as one of
  them; Phase 4 already routes inheritance through `parents`. `reconcile` selects the
  `RollupKind::Reconciliation` kind and the `recon:` id namespace over the same two-parent input.
  Four call sites gain `reconcile: false`.
- **P5-D14 — the paragraph join happens in `rows_from_steps`, not in the renderer, and it joins by
  raw concatenation.** The durable steps of one model step are a SPLIT of one stream: the flush
  boundary is a timer, not a sentence. Joining in the row projection means the streaming path and
  the landed path share one function (which is why the bug survived Phase 3: only the trailing
  group was special-cased), and it keeps `Row` the pane's single source of geometry. The ledger
  keeps every chunk as its own step — the fix is a render fix, and one bullet asserts exactly that.
- **P5-D15 — `claims` is one crate holding a Definition, a Provider and one Consumer (the global
  `propose_claim` tool).** §0.2's "don't split preemptively" against §0.2's seam rule: there is one
  conceivable claims Provider, and the global propose tool is three dozen lines that would
  otherwise be a crate with one file. The leader's shadowing twin lives in `tool-leader`, which is
  a separate crate for a different reason (it must reload with the leader binding).
- **P5-D16 — `Actor::Andrey` is a parameter and an ambient-initiator check, not a capability.**
  §16 makes acceptance his act. The seam refuses any `decide` reached while an agent's ambient
  initiator scope is set, which is exactly the condition "this call is inside a wake". It is a
  guard against accident, not against a hostile in-process caller — §2 is explicit that ambient
  presence is never authorization, and there is no authority boundary inside this process.
- **P5-D17 — the GLOBAL persona section is `lane-scope`'s too.** Shadowing is only demonstrable
  against a twin, and no row contributed a persona section before this phase. Putting both halves
  in one row keeps the pair honest (same `SectionId`, one place to read the rule) and costs one
  optional config field; the leader's persona is separate on purpose, because it moves with the
  leader set rather than with the lane list.

---

## 6. WP-8 status, and the deviations it took at the seam

Written by WP-8 (integration) as it landed. Everything below is a statement about a test that RAN,
or about a plan line that turned out to be wrong.

### Green as of this writing

- `plugins/residents` — multi-lane bootstrap and the dormant lane. `cargo test -p
  bough-plugin-residents`: 12 tests, 0 failed, including the two this package added,
  `tests/catchup.rs::{many_lanes_are_bootstrapped_from_one_list,
  a_dormant_lane_is_resumed_and_never_woken}`.
- `crates/bough/tests/many_agents.rs` — 4/4.
- `crates/bough/tests/dormancy_loops.rs` — 3/3.
- `crates/bough/tests/graph_invariants.rs` — 2/2.
- `crates/bough/tests/leader_swap.rs` — 9/9.
- The whole `-p bough` suite is green with the Phase 5 rows in `bough-base` and the widened
  `residents.bootstrap`: no Phase 0-4 gate regressed.

### D-WP8-1 — the `leader.set` GROUP row does not exist, and could not

The plan's bundle sketch gives `leader.set` a `group:` and no `plugin:`. The COMPOSER refuses
that: `config/compose.rs` returns `ComposeError::MissingPlugin` for any row with no plugin, and
its own test `a_row_naming_no_plugin_is_rejected_by_the_composer` pins the refusal. Decision D18's
"pure group row" is a comment in `config/entry.rs` that the composer never implemented, and Phase
5 touches no kernel.

Nesting `tool.leader` INSIDE the `leader` row was tried next and left `tool.leader` `Inactive`
with an EMPTY unmet set — a child row of an active parent did not resolve the parent's own
`leader` binding. Rather than change the kernel from an integration package, the two rows are
FLAT in `bundles/bough-tui-app.yml`. What is lost is the one-line `disabled: true` on the pair;
what is kept is everything the SWAP gate is about, and `leader_swap.rs` proves it: editing
`leader.config.agent` reloads `leader`, `tool-leader` (which injects `leader`) unloads with the
old binding and reloads against the new one, and all nine bullets pass.

A kernel-side fix — either implementing D18's pure group row, or making a nested child see its
parent's provided keys — is the right repair and belongs to whoever next touches
`crates/bough-kernel`.

### D-WP8-2 — "the old agent loses the tools" is about FOUR tools, not five

`tool-leader::TOOL_NAMES` has five entries, but `propose_claim` also exists GLOBALLY: the `claims`
row registers it for every agent, and the leader's scoped twin SHADOWS it (that shadowing is V6's
own bullet). So `leader_swap.rs` asserts over `LEADER_ONLY` — `adopt_unsorted`,
`draft_requirement`, `propose_structure`, `note_timeline` — and checks at runtime that all four
are names the row really registers. Asserting that `propose_claim` disappears from `sol` would
have been asserting that an ordinary lane loses its claim tool.

### D-WP8-3 — per-agent delivery is proved on the STEP id, not the seq

The map's "each recipient gets its own `mail/delivered` step and seq" reads as though the two seqs
must differ. Seqs are PER TRAJECTORY, so two recipients legitimately share the number 41. What
makes the two deliveries two facts is that they are two steps on two chains, which is what
`many_agents.rs::mail_fans_out_across_lanes_in_a_booted_tree` asserts.

### D-WP8-4 — `15-leader-swap.sh` reads `request/header.tools`, because there is no `/tools`

The SWAP script's bullets are named "the leader tools are offered to…". There is no `/tools`
command and no tool pane to grep — the registered commands are `/agents /claims /dormant /drift
/focus /help /oldfeed /prime /quit /reconsolidate /reset /seal /sleep /supersede /wake` plus
claims' `/accept /edit /reject`. The durable record of what an agent was offered is
`request/header.tools` (§5), so the script DRIVES each turn through the surface and READS the
header the turn left behind. The screen drives; the ledger proves.

### D-WP8-5 — a reboot after any Phase 5 step type failed in `agent-loop`'s crash repair — FIXED

Found by the shell-use suite, reproduced three ways, and fixed during integration.

`agent-loop`'s `apply` runs crash repair BEFORE it publishes the factory, and repair READ every
lane's chain unfiltered. The Phase 5 rows declare their step types in their OWN `apply`, so on the
SECOND boot of a tree that had ever written one, repair met a type nothing had declared yet and the
ledger refused the read (`UnknownStepTypeOnRead`). The row reported `agent.loop … is Failed` and the
binary refused to boot. Reproduced with `agent/dormancy` (after `/sleep`) and `graph/bud` (after
accepting a lane claim).

**The fix** is that repair now names the four types it reasons about — `repair::REPAIR_KINDS` =
`wake/start`, `wake/end`, `tool/call`, `tool/result`, all ledger-core vocabulary and therefore
always declared — in its `StepQuery.kinds`. A row of any other type is never materialized, so an
undeclared one cannot refuse the read. Reordering the bundle was tried and does NOT fix it: row
order is not the mechanism.

That made the two ledger Providers disagree, and the second half of the fix is closing that: the
sqlite provider applies `kinds` as a `WHERE s.type IN (…)`, so the filter is part of what is READ,
while `ledger-memory` applied the unknown-type rule first and only then filtered. `ledger-memory`
now filters by kind before admitting, and the conformance case
`a_kind_restricted_read_never_meets_an_unknown_type` pins the behaviour on both.

### D-INT-1 — `/help` had outgrown the notice band, silently

The whole shell-use suite (not just the four Phase 5 scripts) turned up one more regression:
`05-commands.sh::help_lists_the_registered_commands` looks for `/quit` in `/help`'s output, and
Phase 5's seven new commands pushed the list past the notice band's eight rows. The band did
`.take(h)` — a truncation with no marker, so a command that had vanished off the bottom was
indistinguishable from one that was never registered.

The band is now `pane::notice_band(text, cap, available)`: pure, clamped by BOTH the cap and the
rows actually above the composer, and when it drops lines its last row says how many. The cap
itself (`tui.shell.notice_max_lines`, and the value `bough-tui-app.yml` sets) went from 8 to 24.

### Shell-use tally at hand-off

| script | ok | blocked |
|---|---|---|
| `12-many-agents.sh` | 7 | 3, all behind D-WP8-5 |
| `13-claims.sh` | 7 | 2, both behind D-WP8-5 |
| `14-forks.sh` | 4 | — |
| `15-leader-swap.sh` | 4 | — |

**After integration:** the whole replay suite (`make tui-test-replay`, all fifteen scripts) is
**89 ok, 0 not ok, 0 skipped** — the five bullets behind D-WP8-5 and the one behind D-INT-1
included.

Three script-level facts worth keeping, each found the hard way:

- A pane keeps the keyboard after a click, so a message typed next goes to the pane and not the
  composer. `12` puts the turn BEFORE the click bullets and clicks the composer's placeholder to
  come back; `14` cycles pane focus with `Tab` because `^b` is the trajectory pane's key.
- `/sleep`, `/edit` and `/reject` take POSITIONAL arguments, so a multi-word reason is more
  arguments than the spec admits and the command answers with its usage line. Every reason the
  suite passes is one token.
- The focus pane's title is the word `trajectory` and carries no lane name, so "the pane followed
  the click" is only readable off the CONTENT — `12` plants a marker step on `terra` for it.

---

## 7. Deviations and open items (the closing review pass)

A review of the phase raised 26 findings. What follows is what changed, and what deliberately did
not. The rule throughout: REQUIREMENTS wins, and where the code stands against it and stays, the
spec is amended rather than the disagreement left unrecorded (AGENTS.md).

### 7.1 REQUIREMENTS.md amendments

Three, all made in this pass. The plan had claimed the first of them was already made; it was not.

- **§5, the wake flow** now begins at `-> agent/wake-request`, the admission waterfall both loop
  Providers dispatch immediately before `wake/start`. P5-D1 said this "amends §5's wake flow and is
  flagged as such"; `git log -- REQUIREMENTS.md` showed the file untouched since the initial
  rebuild commit, so the spec of record did not describe the loop that ships. It does now.
- **§4, merge** now states that the new head carries an `ancestor` edge to each parent BESIDE the
  two `merge` edges, and that a reader who means "birth" must exclude a child that has a merge
  edge. `connected()` derives membership from ancestry alone (frozen in Phase 1), so a head joined
  only by merge edges would read neither past — the survivor would lose its own history the moment
  it merged. The deviation was in a code comment; it is now in the spec.
- **§4, undo** now says what "ambiguous" means: a ref two children BOTH claim. A ref neither claims
  is not a tie and stays with the parent. `route.rs`'s module header claimed the opposite of its
  own code and its own test; the header now states the rule the module actually holds.

### 7.2 Fixed

**The leader.**

- `leader.config.attribute_reconsolidation` was set in the bundle and read by nothing:
  `reconsolidation`'s `/reconsolidate` wrote `Attribution::System` unconditionally, so §8's
  leader-attributed pass did not exist and a live config field was inert. `ReconHandle` gained a
  standing-attribution slot installed as an EFFECT (`attribute_to`), and the leader row installs
  its own name into it. It moves with the set and unwinds to `System` on unload, like every other
  leader registration. Pinned by `bough` `tests/leader_swap.rs::the_reconsolidation_pass_is_
  attributed_to_the_leader_and_moves_with_it`.
- `MailHandle::adopt` attributed every adoption to the RECIPIENT lane, so each `mail/adopted` row
  claimed a lane adopted its own mail. `adopt` now takes `by: Attribution` and the leader passes
  its own name.
- A leader question could never reactivate a dormant leader: `ask_leader` routes at
  `MailClass::Wake` on `class:ask`, but no row carried `class:ask` in `routing_refs` and none
  declared it as a wake class, so the question fell to the unsorted queue and was delivered
  `Ordinary`. The leader row now gives its target both, idempotently and with `agent/routing`
  evidence, retrying on `agent/created` when the lane is not born yet. Pinned by
  `tests/leader_swap.rs::the_leader_row_is_routed_and_wakes_on_class_ask`.

**Mail.**

- **The unsorted queue never drained.** `unsorted()` filtered on `kind == "mail/unrouted"` alone,
  so a leader reading the oldest N on every wake read the same N forever: duplicate delivery (the
  crate's own `adoption_names_its_unrouted_step` invariant would report it at the next quiesce) and
  starvation of everything queued behind them. `unsorted()` now excludes items a `mail/adopted`
  step names, paging by seq so the answer is `limit` UNADOPTED items; `adopt` is idempotent.
  Pinned by `mail-router` `tests/unsorted.rs::an_adopted_item_leaves_the_queue_and_a_second_pass_
  is_a_no_op`, and the existing "later sink" test now asserts on `by`.
- A matched recipient with no live handle was silently dropped: no delivery, no ledger row, and
  the name still in `RouteReport.matched`. It now writes the event to the unsorted trajectory —
  §3's recovery surface — and reports `undeliverable`. Pinned by `tests/fanout.rs::a_matched_lane_
  with_no_live_agent_is_recorded_not_dropped`.
- `MailConfig.deliver_to_dormant` was renamed `tolerate_absent_lane`. Dormancy never removes an
  agent's handle, and `true` never meant "deliver" — it meant "do not fail".
- `unsorted_sink` replaced the slot BEFORE registering the effect that restores it, so a failing
  `ctx.effect` evicted the previous sink permanently. The replace now happens inside the effect.
- `one_delivery_per_recipient` grouped by body + `at`, which missed the re-delivery it exists to
  catch and reported two distinct events that shared a timestamp. It groups by body + CITES now.
- **`routing_refs` and `wake_classes` had no configuration surface at all**, which made dormancy's
  wake-class path unreachable outside tests. `MailHandle::set_wake_classes` was added (with
  `agent/routing` evidence, an additive `wake_classes` field on that body), and the leader uses
  both. See 7.4 for what is still missing here.

**Graph ops.**

- Every op wrote and deleted `agents` ROWS behind the live registry's back and nothing reconciled
  them: after a merge the survivor kept appending to its PRE-MERGE chain while its row pointed at
  the new head, the absorbed agent kept running with no row, and a split's children had rows but
  no live agent — so mail matched to their new `routing_refs` hit `by_name(..) == None` and was
  dropped. `graph-ops` now publishes `agents/rows-changed` (a new EMIT event on the `agents`
  Definition) after every op, and `residents` — the row that owns the disposers — reconciles:
  dispose a deleted row's agent, dispose-and-resume a row whose trajectory moved, resume a row
  with no agent. `residents::reconcile_rows` is total and idempotent. This also closes the
  separate finding that only `ClaimKind::Lane` brought its agent up: split, bud and merge now
  reconcile through the same path.
- `GraphConfig.max_children` was a config field whose only legal value was 2, with no `validate()`
  — `max_children: 3` composed, booted, and turned a boot-time typo into a runtime invariant
  violation. It is now the protocol constant `SPLIT_CHILDREN`.
- `question_on_ambiguity` was a test seam shipped as a production field: setting it `false` in any
  patch layer silently removed §4's notification path. **P5-D9 is withdrawn** and the field is
  gone. The test it existed for is replaced by `tests/routing.rs::an_ask_that_fails_still_refuses_
  and_is_not_swallowed`, which drives a failing `LeaderAsk` — a stronger claim, and one that also
  covers the next item.
- `merge::apply`'s no-survivor path did `let _ = refuse(..)`, so a failing mail seam meant the
  caller was told "no survivor" while nobody was ever asked. The ask error now propagates.
- `undo`'s merge-shaped `OpOutcome` hardcoded `edges: 2` while writing 4 per lived-in child. It is
  summed from what `merge_rows` reports.
- `resolve_point` read the WHOLE chain unfiltered while inspecting only `wake/start` / `wake/end`
  — the same D-WP8-5 bug fixed once in `agent-loop`'s repair. So did `worker-fork`'s `fork_point`
  and its invariant. All three now carry a `kinds` filter next to the walker that reads them
  (`seq::WAKE_KINDS`, `point::WAKE_KINDS`, `REQUEST_HEADER`). The two point resolvers take the
  trajectory's true `head` as a parameter, because a filtered chain's last row is not the
  trajectory's last row — pinned by `point.rs::an_empty_chain_has_no_point`'s new clause.
- `tui-focus`'s branch picker filtered on `EdgeKind::Ancestor`, which — given the merge head's
  ancestor edges — showed a merge as a birth. It now excludes any child that has a merge edge to
  the same parent. Pinned by `tui-focus` `tests/branches.rs::a_merge_head_is_not_offered_as_a_
  branch_even_though_it_has_an_ancestor_edge`.
- `tests/routing.rs`'s "the question names BOTH claimants" tested for the CHARACTERS `a` and `b`
  in an English sentence. The children are named `lane-a` / `lane-b` and the assertion is on the
  names.

**The loop, claims, dormancy, drift.**

- `spawn_wake`'s `stopping` early-return did not call `cell.wake_refused()` the way the new
  `Defer` branch does, so a driver stopping in that window left `pending_wake` latched and
  `Agent::when_idle()` never returned. It does now, in both the pre-admission and the new
  post-admission check.
- Making `spawn_wake` async put an `.await` between the `stopping` check and `mint_wake`, so two
  concurrent callers could mint two wakes for one trigger where the sync path serialised them. An
  `admit_gate` is held across admission-and-mint; the wake still runs on its own task.
- **`/edit`, `/reject`, `/sleep` and `/supersede` refused any multi-word text**, despite
  advertising `<text…>` / `<reason…>`: `positional`'s `maxItems` was enforced by `jsonschema`
  before `run` was reached, so the keyboard half of the accept/edit/reject gate could only edit a
  claim to ONE WORD. `commands::positional_rest` was added and those four use it. Pinned by
  `commands` `schema_tests::a_rest_argument_accepts_many_words` and its capped twin; the shell-use
  scripts now pass real sentences and assert the sentence lands in the ledger.
- The lane-birth rollback deleted the `agents` row while the cited `graph/bud` step, the child
  trajectory, the edge and the digest all stood — the exact failure P5-D8's append-last ordering
  exists to prevent. It now UNDOES the bud through `graph.undo` and only then guarantees the row
  is gone; an `AlreadyLive` from the new reconciler is treated as the success it is.
- `drift-watch`'s claim-rejection rate was computed and rendered but read by no `flags()` clause,
  so no rejection rate at any level ever raised a flag. `DriftFlag::ClaimsMostlyRejected` was
  added with `claim_rejection_flag` and `claim_rejection_min_decided` thresholds (a single
  rejected claim is a rate of 1.0 and says nothing). Pinned in `signals.rs`'s flag test.
- `worker-fork`'s `ForkSetup::setup` used `expect("setup runs once")` on a fallible async trait
  boundary; a second invocation panicked inside a tokio task instead of returning
  `AgentError::SetupFailed`. It returns `SetupFailed`.

**Test honesty.** Four shell-use bullets asserted something other than what they were named for,
and are fixed rather than renamed away:

- `13-claims.sh::accepting_a_lane_claim_adds_a_rail_row` asserted `see "vega"` immediately after
  `/accept`, which the command's own `lane born: vega` echo in the notice band satisfies. It now
  asserts the RAIL ROW: `vega` and a state glyph on ONE line.
- `12-many-agents.sh::a_dormant_lane_shows_the_dormant_glyph` never checked the glyph — `see
  "dormant"` was satisfied by `/sleep`'s echo. It now asserts `luna` + `◌` on one row, with the
  word as a second bullet.
- `14-forks.sh::a_lane_child_and_a_fork_child_are_labelled_differently` was `see "lane" && see
  "fork"`, both already on screen from the fixture's own trajectory ids `lane/bud` and
  `traj/fork-of-sol`: it would have passed if `Branch::word()` returned the empty string for both
  kinds. The label is now asserted on the row it labels.
- `12-many-agents.sh`'s "while streaming" bullet polled for up to 30 seconds, by which time the
  durable steps had landed — it asserted the same state as the bullet after it. It is renamed to
  what it tests, a second bullet asserts the first fragment is never a row of its own, and the
  comment says plainly that the mid-stream rule is pinned purely in `tui-focus/tests/stream.rs`.
- `mail-router` `tests/unsorted.rs::with_no_sink_the_queue_keeps_it_and_a_later_sink_adopts_it`
  proved only the first half of its name and never asserted the item left the queue — which is why
  the never-draining queue survived the suite. It now asserts `by`, and the drain is pinned by a
  new sibling.

New helpers `row_with` and `no_row_is_exactly` in `scripts/tui/lib.sh` make "these things are on
ONE line" assertable; `see` matches the SCREEN, so it cannot tell one row from two stacked ones,
and that gap is what three of the four vacuous bullets were made of.

### 7.3 Deliberately NOT changed

- **A split leaves the parent row alive beside its two children.** The review reads §4's "create
  two heads" as requiring the parent row to go, which would make a split leave two lanes rather
  than three. It is not changed, for three reasons: §4 distinguishes bud from split by the POINT
  ("a split whose point is in the past"), not by whether the parent survives; §3 forbids a black
  hole, and a ref no child claimed needs a home, which is what `route.rs`'s `keep` is; and
  `undo::apply` restores the parent from `inner.row(&parent_name)`, so deleting the row on split
  breaks both undo shapes and would need the parent's name and overrides carried in the
  `graph/split` body to rebuild. That is a real change with real risk, and it is not one to make
  in a closing pass. §4's undo bullet was amended to state the rule the code holds instead. **Open
  item for a later phase.**
- **`lane-scope` still cannot give a lane an EXTRA tool.** §5 names three per-lane registrations;
  `LaneSpec` carries two (persona, `tools.restrict`). Scoped tool ADDITION is demonstrated by the
  `leader`/`tool-leader` pair, which is a different mechanism — a named plugin set, not a list
  entry — and adding a tool by config would mean naming an already-registered tool to re-scope,
  which is not what §5's sentence is about either. Left as it is, and recorded here rather than
  quietly.
- **`residents`' catch-up can still race `dormancy`'s admission listener.** The declared-dependency
  fix was written and REVERTED: adding `Inject::optional(["dormancy"])` makes an optional key that
  arrives after activation change the committed view, which reloads the row — and reloading
  `residents` disposes and re-raises the whole roster, leaving three disposed lanes on the strip
  beside the three live ones (`12-many-agents.sh::the_focus_pane_follows_the_click` caught it). The
  loop's `agent/wake-request` admission point still defers the catch-up whenever `dormancy`'s
  listener is registered, and `catch_up` now takes an optional `is_dormant` predicate so a caller
  that holds the handle can skip the request outright. Fixing the ORDER properly needs an
  activation handshake this phase does not have — `residents` already waits for the factory slot,
  and the equivalent for a listener does not exist. **Open item.**
- **`claims::decide::load` and `ClaimsHandle::open` read every `claim/*` step in the ledger** on
  every call, unbounded and untrimmed. Bounding `load` needs an id lookup the ledger has no index
  for, and capping the scan would silently stop finding old claims — a wrong answer instead of a
  slow one. Phase 5 touches no ledger vocabulary or schema, so this waits for a phase that does.
- The mail invariant's "never zero deliveries" half still has no ledger witness for a delivery that
  ERRORED mid-fan-out; nothing records an intent to deliver. The one case the router can observe
  on its own — a matched row with no live agent — is now durable, and the module doc says exactly
  that much and no more.
- `graph-ops`' test double `RecordingDigests::rebuild_digest` reimplements the P5-D13 id/kind
  mapping it is used to verify. The production mapping is separately pinned by `rollups`'
  conformance case `a_reconciliation_digest_is_its_own_kind_and_namespace`, run against both
  Providers, so this is a coupling smell rather than a hole. Left.

---

## 8. Found by the track-B merge (2026-08-28)

Three Phase 5 defects that only the merge could see, all fixed on the merge commit. The full
accounting is `docs/track-b-merge-notes.md` § "What the merge itself had to fix".

- **`worker-fork` could not fork in the shipped bundle at all.** `ForkSetup::setup` pinned the
  child's prefix through `prefix::pin(agent.ctx(), …)`, which resolved `Projection` off the CHILD
  AGENT's context — a context owned by the `agents` row, which does not declare `projection` in
  its `inject`. Every `WorkerKind::Fork` in `bundles/bough-base.yml` therefore died in setup with
  *plugin `agents` (row `agents`) read service `projection` without declaring it*. Nothing caught
  it because `plugins/worker-fork`'s own fixtures mount their own `agents`, and no test started a
  fork against the real bundle — until track B's tripwire
  (`boundary_injection.rs::no_fork_path_exists_to_assert_on_yet`) forced one to be written. The
  handle is now passed into `prefix::pin` from the fork ROW, which already holds it; the effect
  still belongs to the child, so P5-D12 is unchanged.
- **`residents`' async roster is not something a test may read once.** The registry entry and the
  `agents` ledger row do not land in the same instant, and a boot-time reload of the `agents` row
  REPLACES the registry — so a handle captured from `peek_live` before that reload stays empty
  for ever. `phase6_swap.rs::resident` now polls, re-peeking both handles each pass. This is the
  same window merge note 16 measured from the composer's end and §7.3's open item names from
  `residents`' own end.
- **Mail routed by refs means a test lane has to be LINKED.** Two `phase6_swap` gates read `sol`'s
  trajectory after a collector sweep with nothing linking `sol` to the repository; with `mail` in
  the tree the items go to the unsorted queue and reached `sol` only when the leader's adoption
  pass happened to run first. `deliver_to` in a layer is no longer a destination when a router is
  mounted, and a test that means "this lane gets this repository's mail" has to say so.
