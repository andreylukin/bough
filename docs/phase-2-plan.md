# Phase 2 — one resident agent, end to end: design and work breakdown

REQUIREMENTS §17 Phase 2, resting on §2 (the whole of it), §5 (the whole of it), §7, §10, §12, §9's
tools paragraph, and §0 throughout. Phase 0 built the center, Phase 1 built the ledger and the
projection seam; this phase builds everything between a message and a model call and back:

```
agents (Definition)  ── agent-loop (Provider) ──┬── llm (Definition) ── llm-anthropic | llm-replay
   handle, registry,      the §5 wake flow      │        │
   inbox, scope,          preemption, mail,     │        └── llm-retry (agent/request-error)
   agent/* vocabulary     crash repair          │
                                                ├── tools (Definition) ── tools-baseline
                                                ├── workers (Definition) ── worker-spawn
                                                └── actions (Definition) ── (no providers: Phase 6)
   model-policy, about-line: plugins on agent/request and the wake-end moment, never loop code.
   agent-loop-scripted: a second Provider of the same seam, the phase's swap gate.
```

The phase-shaped rule, restated because every work package below is judged by it: **nothing in this
phase may add loop code outside `agent-loop` / `agent-loop-scripted`, and nothing model-visible may
arrive except as a ledger step.** Both are checked by invariant modules, not by review alone.

This document is normative for signatures. An implementer may add private items freely and may not
change a signature here without editing this document first. Everything is `Send + Sync + 'static`;
one tokio runtime.

---

## 1. Crates

Seventeen new crates under `plugins/`, each `bough-plugin-<name>`, plus edits to four Phase-1 crates
and to the launcher. §15 item 6's granularity review at phase close applies: any crate here with one
provider, one consumer and no second on the horizon folds back into its neighbour in Phase 3.

| crate (`plugins/…`) | package | catalog row(s) | provides | injects | role |
|---|---|---|---|---|---|
| `llm` | `bough-plugin-llm` | `llm` | `llm` | — | **Definition** §12: message + stream vocabulary, adapter registry, `agent/request`, `agent/request-error`, `llm/stream` |
| `llm-anthropic` | `bough-plugin-llm-anthropic` | `llm-anthropic` | — | `llm` | **Provider**: wraps `bough_llm::client_for` |
| `llm-replay` | `bough-plugin-llm-replay` | `llm-replay` | — | `llm` | **Provider**: answers from a recorded transcript |
| `llm-retry` | `bough-plugin-llm-retry` | `llm-retry` | — | `llm` | **Consumer**: waterfall listener on `agent/request-error` (backon) |
| `agents` | `bough-plugin-agents` | `agents` | `agents` | `ledger` | **Definition** §2: handle, session, inbox, registry, factory slot, initiator scope, `agent/*` |
| `agent-loop` | `bough-plugin-agent-loop` | `agent-loop` | — | `agents`, `ledger`, `projection`, `llm`, `tools` | **Provider**: the §5 wake flow. The only crate with concrete loop code |
| `agent-loop-scripted` | `bough-plugin-agent-loop-scripted` | `agent-loop-scripted` | — | `agents`, `ledger`, `projection`, `tools` | **Provider**: replays a fixed transcript through the same seam |
| `tools` | `bough-plugin-tools` | `tools` | `tools` | — (optional `approval`) | **Definition** §9: scoped registry + guarded pipeline, `tools/*` |
| `tools-baseline` | `bough-plugin-tools-baseline` | `tools-baseline` | — | `tools` | **Consumer**: `bash`, `read_file`, `write_file`, `edit_file`, `glob`, `grep` |
| `workers` | `bough-plugin-workers` | `workers` | `workers` | `ledger` | **Definition** §10: start/result vocabulary, live runs, bounds, provider registry |
| `worker-spawn` | `bough-plugin-worker-spawn` | `worker-spawn` | — | `workers`, `agents`, `ledger`, `tools` | **Provider**: fresh task-only context through the agent factory |
| `tool-workers` | `bough-plugin-tool-workers` | `tool-spawn_worker`, `tool-ask` | — | `workers`, `tools` | **Consumers**: the two model-facing tools (one crate, two rows) |
| `actions` | `bough-plugin-actions` | `actions` | `actions` | `ledger` | **Definition** §7: the four kinds, the idempotency journal, `actions/execute`, reconciliation |
| `tool-actions` | `bough-plugin-tool-actions` | `tool-actions` | — | `actions`, `tools` | **Consumer**: registers the four primitives as tools |
| `model-policy` | `bough-plugin-model-policy` | `model-policy` | — | `llm`, `ledger` | **Consumer** §12: prepend listener on `agent/request` |
| `about-line` | `bough-plugin-about-line` | `about-line` | — | `agents`, `ledger`, `projection` | **Consumer** §2: the two-half about-line, a listener + a projection section |
| `exec-headless` | `bough-plugin-exec-headless` | `exec` | — | `agents`, `ledger` | **Consumer**: `bough exec` runs one task and asks the process to exit |

Edited Phase-1 crates (owner: WP-1 only, so the file sets below stay disjoint):
`plugins/ledger` (two vocabulary additions + `action_done(at)`), `plugins/ledger-sqlite`,
`plugins/ledger-memory`, `plugins/projection` + `plugins/projection-assembler` (`as_of`).
Edited center: `crates/bough-kernel` (one exit signal), `crates/bough` (the `exec` subcommand).

New workspace dependencies (§13 already names all of them): `backon` (llm-retry), `jsonschema`
(worker seals — Phase 0 open item 6 closes here), `tokio-util` (cancellation), `futures`,
`sha2` (idem keys, request digests), `similar` is **not** needed yet (Phase 3 renders diffs).

**Dependency direction.** `agents` depends on `llm` for the request vocabulary (§12 puts
`agent/request` in the llm Definition, §2 puts the `agent/*` names in agents; the types live in
`llm` and are re-exported from `agents`, P2-D3). Nothing depends on `agent-loop`. `llm`, `tools`,
`workers` and `actions` depend on `ledger` for id types only.

---

## 2. Public API

### 2.1 The model seam (`plugins/llm/src/…`)

```rust
// lib.rs
pub struct Llm;
impl ServiceKey for Llm { type Value = LlmHandle; const NAME: &'static str = "llm"; }
#[derive(Clone)] pub struct LlmHandle(Arc<LlmInner>);

// The message vocabulary IS bough-llm's, re-exported (P2-D2): one set of types, so llm-anthropic's
// mapper is a stream mapper and nothing else, and V4's byte-for-byte comparison has one shape.
pub use bough_llm::types::{Effort, LlmContentBlock, LlmMessage, LlmRole, LlmToolDef, Usage};

bough_util::brand_id!(pub struct AdapterName;);

#[derive(Clone, Debug, PartialEq)]
pub struct LlmRequest {
    pub model: String,
    /// The STABLE prefix (bough-llm's cache contract). The loop puts the projection here.
    pub system: Option<String>,
    pub system_volatile: Option<String>,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<LlmToolDef>,
    pub call: CallConfig,
}
impl LlmRequest {
    /// Canonical JSON of the whole request; the unit of V4's comparison and of `request/header`'s
    /// `projection_digest` sibling. Stable field order, no clock, no ids minted here.
    pub fn canonical(&self) -> String;
    pub fn digest(&self) -> String;               // sha256 of `canonical()`
}

/// The ONLY thing `agent/request` listeners may write (§5: the waterfall is over the call config).
#[derive(Clone, Debug, PartialEq)]
pub struct CallConfig {
    pub model: String,
    pub max_tokens: i64,
    pub effort: Option<Effort>,
    pub tool_choice_none: bool,
    /// Metering, budget notes, anything a listener wants to carry to the next listener.
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// Read-only facts a policy listener needs. The loop re-installs its own copy after the waterfall,
/// so a listener that swaps the Arc changes nothing (P2-D4).
#[derive(Clone, Debug, PartialEq)]
pub struct RequestFacts {
    pub agent: AgentName,
    pub traj: TrajId,
    pub wake: WakeId,
    pub wake_kind: WakeKind,
    pub step_index: u32,
    pub answers_andrey: bool,
    pub model_override: Option<String>,     // agents.model_override, read from the ledger row
    pub prompt_ver: String,
    pub composition: String,                // the composition fingerprint (§0.5)
}
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WakeKind { Answer, Drain, Scheduled, Catchup, Task }

#[derive(Clone, Debug)]
pub struct RequestCall { pub facts: Arc<RequestFacts>, pub call: CallConfig }

/// §5/§12: a waterfall over the CALL CONFIG only. Declared here, re-exported from `agents`.
pub struct AgentRequest;
impl WaterfallEvent for AgentRequest {
    const NAME: &'static str = "agent/request"; type Value = RequestCall;
}

/// §5: chunks append as thought steps as they are produced. A listener may observe or replace the
/// stream; the innermost hop is the resolved adapter.
pub struct LlmStreamEvent;
impl WaterfallEvent for LlmStreamEvent {
    const NAME: &'static str = "llm/stream"; type Value = StreamCall;
}
pub struct StreamCall {
    pub request: Arc<LlmRequest>,
    pub cancel: CancellationToken,
    /// `None` until the innermost hop fills it. A wrapper that returns without calling `next`
    /// must fill it itself or the executor turns the empty value into a `Failed` chunk.
    pub stream: Option<LlmStream>,
}
pub type LlmStream = futures::stream::BoxStream<'static, Chunk>;

/// §12: model failures are TERMINAL CHUNKS, never thrown. Exactly one terminal chunk ends a stream.
#[derive(Clone, Debug, PartialEq)]
pub enum Chunk {
    TextDelta { text: String },
    ReasoningDelta { text: String, meta: Option<serde_json::Value> },
    ToolCall { id: ToolCallId, name: ToolName, input: serde_json::Value },
    Usage(Usage),
    End { stop: StopReason },                     // terminal
    Failed(LlmFailure),                           // terminal
}
impl Chunk { pub fn is_terminal(&self) -> bool; }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason { EndTurn, ToolUse, MaxTokens, StopSequence }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LlmFailure {
    pub kind: FailureKind, pub message: String,
    pub retryable: bool, pub status: Option<u16>, pub adapter: AdapterName,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind { Transport, RateLimit, Overloaded, ContextOverflow, Auth, BadRequest,
                       Cancelled, Truncated, Other }

/// §5: a listener that owns recovery returns `Recovery::Retry(..)` WITHOUT calling `next()`; the
/// default (the executor's own innermost hop) leaves the failure terminal for this wake.
pub struct AgentRequestError;
impl WaterfallEvent for AgentRequestError {
    const NAME: &'static str = "agent/request-error"; type Value = RequestErrorCall;
}
pub struct RequestErrorCall {
    pub facts: Arc<RequestFacts>,
    pub request: Arc<LlmRequest>,
    pub failure: LlmFailure,
    pub attempt: u32,
    pub recovery: Recovery,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Recovery { Terminal, Retry { after: Duration, request: Option<Arc<LlmRequest>> } }

#[derive(Clone)]
pub struct AdapterSpec {
    pub name: AdapterName,
    pub matches: ModelMatch,
    pub adapter: Arc<dyn LlmAdapter>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelMatch { Exact(String), Prefix(String), Any }
impl ModelMatch { pub fn specificity(&self) -> u8; }   // Exact 2 > Prefix 1 > Any 0

#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync + 'static {
    fn name(&self) -> AdapterName;
    /// Never returns `Err`: a failure is the stream's terminal `Chunk::Failed` (§12).
    async fn start(&self, req: Arc<LlmRequest>, cancel: CancellationToken) -> LlmStream;
}

impl LlmHandle {
    /// Registration is an effect (§0.2).
    pub async fn adapter(&self, ctx: &Context, spec: AdapterSpec)
        -> Result<EffectHandle, PluginError>;
    /// Explicit `resolve(request) -> Spec` (§0.2): most specific match wins; a tie is an error
    /// naming both adapters, never a silent last-wins.
    pub fn resolve(&self, model: &str) -> Result<Arc<dyn LlmAdapter>, LlmSeamError>;
    /// Runs the `llm/stream` waterfall and hands back the stream. Adapter failures are chunks;
    /// a missing adapter is one too, so no caller has to branch on two failure shapes.
    pub async fn stream(&self, ctx: &Context, req: Arc<LlmRequest>, cancel: CancellationToken)
        -> LlmStream;
    pub fn adapters(&self) -> Vec<(AdapterName, ModelMatch)>;
}

#[derive(Debug, thiserror::Error)]
pub enum LlmSeamError {
    #[error("no adapter matches model `{model}`; registered: {registered:?}")]
    NoAdapter { model: String, registered: Vec<String> },
    #[error("model `{model}` is matched equally by adapters `{a}` and `{b}`")]
    AmbiguousAdapter { model: String, a: AdapterName, b: AdapterName },
}
```

`llm-anthropic` config: `{ models: String (a `ModelMatch` spelling, default `"*"`),
api_key_env: String (default `ANTHROPIC_API_KEY`), base_url: Option<String>, request_timeout_ms:
u64 }`. It wraps `bough_llm::client_for(model, ClientOpts { retry: RetryOpts::none(), .. })` —
retries belong to `llm-retry`, not to two layers (P2-D5) — and maps `LlmClient::run` +
`on_text` onto the chunk vocabulary: text deltas stream live through the callback; tool calls,
reasoning blocks and usage arrive when the round returns, because that is all bough-llm's surface
exposes (P2-D6). An absent API key is a `Chunk::Failed { kind: Auth }` at call time, never a boot
failure (P2-D7).

`llm-replay` config: `{ transcript: PathBuf | inline rounds, strict: bool }`. A round is
`{ match: Option<String> (substring the last user message must contain), chunks: Vec<Chunk> }`;
`strict: true` (the default) makes an unmatched request a `Chunk::Failed { kind: BadRequest }`
rather than a silent empty answer.

`llm-retry` config: `{ max_attempts: u32, min_delay_ms: u64, max_delay_ms: u64, jitter: bool,
retry_on: Vec<FailureKind> }`. It is a waterfall listener on `agent/request-error` that sets
`recovery = Recovery::Retry { .. }` and returns **without** calling `next()` when the failure is
retryable and attempts remain; otherwise it delegates.

### 2.2 The agents seam (`plugins/agents/src/…`)

```rust
// lib.rs
pub struct Agents;
impl ServiceKey for Agents { type Value = AgentsHandle; const NAME: &'static str = "agents"; }
#[derive(Clone)] pub struct AgentsHandle(Arc<AgentsInner>);

bough_util::brand_id!(pub struct AgentId;);       // the live handle's identity
bough_util::brand_id!(pub struct SessionId;);
bough_util::brand_id!(pub struct MessageId;);     // identifies insertion, claim and discard (§2)

pub use bough_plugin_llm::{AgentRequest, AgentRequestError, CallConfig, RequestCall,
                           RequestFacts, WakeKind};      // §2's request vocabulary, one import

// ---- the handle (§2, verbatim shape) --------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")] pub enum Status { Idle, Running }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")] pub enum CancelCause { User, Parent, Hook, Disposed }

#[derive(Clone, Debug, PartialEq)]
pub struct Session { pub id: SessionId, pub traj: TrajId, pub created_at: DateTime<Utc> }

#[derive(Clone)] pub struct Agent(Arc<AgentInner>);
impl Agent {
    pub fn id(&self) -> &AgentId;
    pub fn name(&self) -> &AgentName;
    pub fn kind(&self) -> AgentKind;                 // Resident | Worker | Fork
    pub fn session(&self) -> &Session;
    pub fn traj(&self) -> &TrajId;
    pub fn inbox(&self) -> &Inbox;
    pub fn status(&self) -> Status;
    /// The agent's SCOPE (§5): scoped tools, sections and `tools.restrict` register through it and
    /// unwind with the agent.
    pub fn ctx(&self) -> &Context;
    pub fn scope_key(&self) -> &ScopeKey;
    /// First cause wins; nothing active ⇒ a no-op that never arms later work; `Disposed` never
    /// latches a pending wake (§2).
    pub async fn cancel(&self, cause: CancelCause, keep_inbox: bool);
    pub fn cancelled_by(&self) -> Option<CancelCause>;
    pub async fn when_idle(&self);
    /// Every inbox mutation is a durable `inbox/spliced` step keyed by the message id (§2).
    pub async fn send(&self, msg: Message, target: Target, wake: bool)
        -> Result<InboxReceipt, AgentError>;
    // The three presets of §2, spelled out so callers never assemble (target, wake) by hand.
    pub async fn followup(&self, msg: Message) -> Result<InboxReceipt, AgentError>; // NextWake+wake
    pub async fn steer(&self, msg: Message)    -> Result<InboxReceipt, AgentError>; // NextStep+wake
    pub async fn inject(&self, msg: Message)   -> Result<InboxReceipt, AgentError>; // NextStep, no wake
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")] pub enum AgentKind { Resident, Worker, Fork }
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")] pub enum Target { NextWake, NextStep }

// ---- mail ------------------------------------------------------------------------------------
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub from: Sender,
    pub class: MailClass,          // ledger vocabulary: Wake | Ordinary (§5's two urgencies)
    pub text: String,
    pub subject: String,
    pub cites: Vec<Cite>,
    pub refs: BTreeSet<Ref>,
    /// Set when this message is DELIVERED mail with a `mail/delivered` step; consumption is
    /// per (agent, seq) and applies to delivered mail only (§5).
    pub mail_seq: Option<Seq>,
    pub at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Sender { Andrey, Agent(AgentName), Worker(WorkerId), Collector(String), System(&'static str) }
impl Message {
    /// The one predicate §5's "an Andrey message ALWAYS gets a fresh sol answer wake" turns on.
    pub fn is_andrey(&self) -> bool;              // from == Sender::Andrey
}

pub struct Inbox { /* .. */ }
impl Inbox {
    pub async fn insert(&self, msg: Message, target: Target) -> Result<InboxReceipt, AgentError>;
    pub fn pending(&self, target: Target) -> Vec<Message>;
    pub fn has(&self, target: Target) -> bool;
    pub fn len(&self) -> usize;
    /// Pure fold over `inbox/spliced` steps: insert minus claim minus discard. Used at resume and
    /// by crash repair; the live inbox and the ledger can never disagree (P2-D8).
    pub fn rebuild(steps: &[Step]) -> Vec<(Message, Target)>;
}
#[derive(Clone, Debug, PartialEq)]
pub struct InboxReceipt { pub message: MessageId, pub agent: AgentId, pub target: Target,
                          pub wake: bool, pub step: StepId, pub seq: Seq }
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedMessage { pub message: Message, pub target: Target, pub claim_step: StepId }

// ---- creation as a transaction (§2) ----------------------------------------------------------
pub struct CreateAgent {
    pub name: AgentName, pub traj: TrajId, pub kind: AgentKind,
    /// Defaults to `ScopeKey::new(format!("agent:{name}"))` via `resolve_create`.
    pub scope: Option<ScopeKey>,
    pub setup: Option<Arc<dyn AgentSetup>>,
    pub seed: Vec<(Message, Target)>,
    pub at: DateTime<Utc>,
}
pub struct ResumeAgent { pub name: AgentName, pub at: DateTime<Utc>,
                         pub setup: Option<Arc<dyn AgentSetup>> }
#[async_trait::async_trait]
pub trait AgentSetup: Send + Sync + 'static {
    /// Runs while BOTH ids are still unpublished (§2). An `Err` rolls the whole creation back.
    async fn setup(&self, agent: &Agent) -> Result<(), AgentError>;
}
/// The CAPABILITY of §2: only its holder can tear the agent down. Not `Clone`.
pub struct AgentDisposer { /* .. */ }
impl AgentDisposer {
    /// Teardown order, normative: stop and drain → unwind scope → detach agent → detach session.
    pub async fn dispose(self);
    pub fn agent(&self) -> &Agent;
}

#[async_trait::async_trait]
pub trait AgentFactory: Send + Sync + 'static {
    fn driver(&self) -> &'static str;
    /// The session, the scope and the handle exist; the registry entry does not yet.
    async fn attach(&self, cell: AgentCell, mode: Attach) -> Result<Arc<dyn AgentDriver>, AgentError>;
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Attach { Created, Resumed }

#[async_trait::async_trait]
pub trait AgentDriver: Send + Sync + 'static {
    fn driver(&self) -> &'static str;
    /// A durable inbox mutation landed: schedule (or not) per target + wake flag and urgency.
    async fn notify(&self, receipt: &InboxReceipt, msg: &Message);
    async fn cancel(&self, cause: CancelCause, keep_inbox: bool);
    /// Stop and drain: no new wake starts, the in-flight wake ends, returns when idle.
    async fn stop(&self);
}

/// The driver's private view: the only way to publish status or claim inbox items.
pub struct AgentCell { /* .. */ }
impl AgentCell {
    pub fn agent(&self) -> &Agent;
    pub fn ledger(&self) -> &LedgerHandle;
    /// Refuses a repeat (`Running → Running`): the agents invariant is enforced at the setter,
    /// not only observed (P2-D9). Emits `agent/status`.
    pub async fn set_status(&self, to: Status) -> Result<(), AgentError>;
    /// A pure DELETION splice (§5): appends one `inbox/spliced { op: claim }` per message.
    pub async fn claim(&self, sel: ClaimSelector, wake: WakeId, at: DateTime<Utc>)
        -> Result<Vec<ClaimedMessage>, AgentError>;
    pub async fn discard(&self, id: &MessageId, wake: WakeId, reason: &str, at: DateTime<Utc>)
        -> Result<(), AgentError>;
    pub fn cancel_token(&self) -> CancellationToken;
}
#[derive(Clone, Debug)]
pub struct ClaimSelector { pub target: Target, pub only: Option<Vec<MessageId>>,
                           pub classes: Option<Vec<MailClass>>, pub limit: Option<usize> }

impl AgentsHandle {
    /// §2: throws if one is already set. The token is an effect, so unloading the driver row frees
    /// the slot and another loop provider can take it (this is what makes the swap test possible).
    pub async fn set_factory(&self, ctx: &Context, f: Arc<dyn AgentFactory>)
        -> Result<EffectHandle, AgentError>;
    pub fn factory(&self) -> Option<Arc<dyn AgentFactory>>;
    pub async fn create(&self, req: CreateAgent) -> Result<(Agent, AgentDisposer), AgentError>;
    pub async fn resume(&self, req: ResumeAgent) -> Result<(Agent, AgentDisposer), AgentError>;
    pub fn get(&self, id: &AgentId) -> Option<Agent>;
    pub fn by_name(&self, name: &AgentName) -> Option<Agent>;
    pub fn list(&self) -> Vec<Agent>;
    pub fn resolve_create(&self, req: &CreateAgent) -> CreateSpec;     // the explicit defaulting step
}

/// §2's ambient initiator: ATTRIBUTION ONLY, never authorization. Nothing in this phase reads it
/// to make a decision; the journal and mail routing read it to write a name.
pub mod initiator {
    pub fn current() -> Option<AgentId>;
    pub async fn with<F: Future>(id: AgentId, fut: F) -> F::Output;
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("an agent factory is already set by driver `{0}`")] FactoryAlreadySet(&'static str),
    #[error("no agent factory is set; mount an `agent-loop` row")] NoFactory,
    #[error("agent `{0}` is already live")] AlreadyLive(AgentName),
    #[error("no live agent named `{0}`")] NoSuchAgent(AgentName),
    #[error("setup for agent `{name}` failed, and the creation was rolled back: {detail}")]
    SetupFailed { name: AgentName, detail: String },
    #[error("agent `{name}` is disposed")] Disposed { name: AgentName },
    #[error("status `{0:?}` repeats the current status")] StatusRepeat(Status),
    #[error(transparent)] Ledger(#[from] LedgerError),
}
```

**The `agent/*` event catalog** (§0.2: each declares its dispatch mode as part of its contract).
LIVE events carry a live handle and are never the durable record of anything.

| event | type | mode | payload / value | meaning |
|---|---|---|---|---|
| `agent/created` | `AgentCreated` | Emit | `Agent` | the creation transaction committed |
| `agent/disposed` | `AgentDisposed` | Emit | `AgentId` | teardown finished |
| `agent/status` | `AgentStatusChanged` | Emit | `StatusChange { agent, from, to }` | never repeats |
| `agent/inbox` | `AgentInbox` | Emit | `(InboxReceipt, Message)` | a durable splice landed |
| `agent/wake` | `AgentWake` | Emit | `WakeEvent { agent, wake, kind, phase }` | live mirror of `wake/start` / `wake/end`; the durable fact is the step |
| `agent/step` | `AgentStep` | Emit | `StepEvent { agent, wake, index, phase }` | live mirror of `step/start` / `step/end` |
| `agent/pre-step` | `AgentPreStep` | **Waterfall** | `PreStep` | §5: reject \| enter(messages) |
| `agent/request` | `AgentRequest` | **Waterfall** | `RequestCall` | call config only (§2.1) |
| `agent/request-error` | `AgentRequestError` | **Waterfall** | `RequestErrorCall` | recovery (§2.1) |
| `agent/wake-stopping` | `AgentWakeStopping` | **Serial** | `WakeStopping`, `Output = Infallible` | every listener runs, in order; data decides (P2-D10) |
| `agent/wake-end` | `AgentWakeEnd` | **Parallel** | `WakeEnded { agent, wake, reason, summary, end_step }` | dispatched for COMPLETED wakes only; where the about-line refresh happens (P2-D11) |
| `agent/preempt` | `AgentPreempt` | Emit | `Preempt { agent, interrupted: WakeId, by: MessageId, answer: WakeId }` | §5 checkpoint-and-answer |
| `agent/continuation` | `AgentContinuation` | Emit | `Continuation { agent, wake, from_jot: StepId }` | a wake resumed from a jot |

```rust
pub struct PreStep {
    pub agent: AgentId, pub name: AgentName, pub wake: WakeId, pub kind: WakeKind,
    pub step_index: u32,
    pub claimed: Vec<ClaimedMessage>,        // read-only: the claim is already durable
    pub decision: PreStepDecision,
}
#[derive(Clone, Debug, PartialEq)]
pub enum PreStepDecision {
    /// Messages the model will see for this step. Claimed messages the decision omits STAY
    /// REMOVED (§5) — they are already spliced out and are the omitter's problem.
    Enter { messages: Vec<LlmMessage> },
    Reject { reason: String },
}
pub struct WakeStopping { pub agent: AgentId, pub wake: WakeId, pub kind: WakeKind,
                          pub steps: u32, pub concludes: bool, pub handle: Agent }
```

### 2.3 The tools seam (`plugins/tools/src/…`)

```rust
pub struct Tools;
impl ServiceKey for Tools { type Value = ToolsHandle; const NAME: &'static str = "tools"; }
#[derive(Clone)] pub struct ToolsHandle(Arc<ToolsInner>);

bough_util::brand_id!(pub struct ToolName;);
bough_util::brand_id!(pub struct ToolCallId;);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderIntent { Generic, Terminal, Diff }     // decided up front (§9)

#[derive(Clone)]
pub struct ToolSpec {
    pub name: ToolName,
    pub description: String,
    pub input_schema: schemars::Schema,
    pub render: RenderIntent,
    pub scope: ToolScope,                              // Global | Agent(AgentName)
    pub tool: Arc<dyn Tool>,
}
#[derive(Clone, Debug, PartialEq)] pub enum ToolScope { Global, Agent(AgentName) }

#[async_trait::async_trait]
pub trait Tool: Send + Sync + 'static {
    /// EXACTLY `true` permits parallel dispatch; everything else is exclusive and forms a barrier.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool { false }
    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall { pub id: ToolCallId, pub name: ToolName, pub args: serde_json::Value,
                      pub agent: AgentName, pub wake: WakeId, pub step_index: u32 }
pub struct ToolCx { pub ctx: Context, pub cancel: CancellationToken,
                    pub deadline: Option<Instant>, pub initiator: Option<AgentId> }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ToolOutcome { pub content: String, pub value: Option<serde_json::Value>,
                         pub cites: Vec<Cite>, pub concludes_wake: bool }
#[derive(Clone, Debug, PartialEq)]
pub struct ToolFailure { pub kind: FailureClass, pub message: String }
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass { NotFound, Denied, Blocked, Timeout, Cancelled, Unknown, Error }

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub call: ToolCallId, pub name: ToolName,
    pub ok: bool,
    pub content: String,
    /// `accept` may replace content OR value, never both (§9); `block` yields a VALUELESS failure.
    pub value: Option<serde_json::Value>,
    pub attached: Vec<AttachedContext>,
    pub cites: Vec<Cite>,
    pub concludes_wake: bool,
    pub failure: Option<ToolFailure>,
    pub started_at: DateTime<Utc>, pub ended_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AttachedContext { pub id: String, pub text: String }

// ---- the three-stage pipeline (§9) ------------------------------------------------------------
/// MONOTONIC by construction (P2-D12): the only public mutators tighten. There is no `allow()`.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision { Allow, Ask { reason: String }, Deny { reason: String } }
pub struct PreExecute { pub call: Arc<ToolCall>, decision: Decision, pub agent: AgentName }
impl PreExecute {
    pub fn decision(&self) -> &Decision;
    pub fn deny(&mut self, reason: impl Into<String>);   // Allow|Ask|Deny -> Deny
    pub fn ask(&mut self, reason: impl Into<String>);    // Allow -> Ask; Deny stays Deny
}
pub struct ToolsPreExecute;
impl WaterfallEvent for ToolsPreExecute {
    const NAME: &'static str = "tools/pre-execute"; type Value = PreExecute;
}

/// Around-dispatch. A wrapper may replace ONLY the cancellation signal, and deadlines WRAP
/// (`min`), never lengthen. `call` is compared by digest after the chain and any edit is ignored
/// and logged: §9 does not offer input rewrite.
pub struct Execution {
    pub call: Arc<ToolCall>,
    pub cancel: CancellationToken,
    pub deadline: Option<Instant>,
    pub outcome: Option<Result<ToolOutcome, ToolFailure>>,
}
pub struct ToolsExecute;
impl WaterfallEvent for ToolsExecute {
    const NAME: &'static str = "tools/execute"; type Value = Execution;
}

pub struct PostExecute { pub call: Arc<ToolCall>, result: ToolResult }
impl PostExecute {
    pub fn result(&self) -> &ToolResult;
    pub fn accept_content(&mut self, content: String);          // clears `value`
    pub fn accept_value(&mut self, value: serde_json::Value);   // clears `content`
    pub fn attach(&mut self, ctx: AttachedContext);
    pub fn block(&mut self, reason: impl Into<String>);         // valueless failure
}
pub struct ToolsPostExecute;
impl WaterfallEvent for ToolsPostExecute {
    const NAME: &'static str = "tools/post-execute"; type Value = PostExecute;
}

/// Emit, observe-only, immutable (§9).
pub struct ToolsResult;
impl EmitEvent for ToolsResult { const NAME: &'static str = "tools/result"; type Payload = Arc<ToolResult>; }

// ---- the registry -----------------------------------------------------------------------------
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Restrict { pub allow: Option<BTreeSet<ToolName>>, pub deny: BTreeSet<ToolName> }
impl Restrict { pub fn intersect(&self, other: &Restrict) -> Restrict; }

impl ToolsHandle {
    pub async fn register(&self, ctx: &Context, spec: ToolSpec) -> Result<EffectHandle, PluginError>;
    /// §5: an INTERSECTION filter over the global set, registered in the agent's scope.
    pub async fn restrict(&self, ctx: &Context, agent: &AgentName, r: Restrict)
        -> Result<EffectHandle, PluginError>;
    /// EXACTLY what the prompt shows. Scoped tools shadow same-named globals for that agent alone.
    pub fn schemas(&self, agent: &AgentName) -> Vec<LlmToolDef>;
    pub fn visible(&self, agent: &AgentName) -> Vec<ToolName>;
    /// A filtered-away tool answers `NotFound`, indistinguishably from a nonexistent one (§9).
    pub fn resolve(&self, agent: &AgentName, name: &ToolName) -> Result<Arc<dyn Tool>, ToolsError>;
    /// The guarded pipeline. Concurrency-safe calls dispatch in parallel, everything else forms a
    /// barrier; only DISPATCH overlaps — the returned results are in the model's call order.
    pub async fn execute(&self, ctx: &Context, calls: Vec<ToolCall>) -> Vec<ToolResult>;
    pub fn approval(&self) -> Option<ApprovalHandle>;
}

/// Declared here, mounted by nobody in Phase 2 (§9: `ask` degrades to deny when absent).
pub struct Approval;
impl ServiceKey for Approval { type Value = ApprovalHandle; const NAME: &'static str = "approval"; }
#[derive(Clone)] pub struct ApprovalHandle(Arc<dyn Approver>);
#[async_trait::async_trait]
pub trait Approver: Send + Sync + 'static {
    async fn ask(&self, call: &ToolCall, reason: &str) -> ApprovalOutcome;
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum ApprovalOutcome { Allow, Deny }

#[derive(Debug, thiserror::Error)]
pub enum ToolsError {
    #[error("no tool named `{name}` is available to agent `{agent}`")]
    NotFound { name: ToolName, agent: AgentName },
    #[error("tool `{name}` is already registered {scope}")] Duplicate { name: ToolName, scope: String },
}
```

`tools-baseline` config: `{ root: PathBuf, bash_timeout_ms: u64, max_output_bytes: usize,
max_read_bytes: usize, deny_globs: Vec<String> }`. Six tools: `bash` (Terminal render, never
concurrency-safe), `read_file` / `glob` / `grep` (Generic, concurrency-safe), `write_file` /
`edit_file` (Diff render, not concurrency-safe). `root` is a containment check, not a sandbox: §7
says no sandbox, and the check exists so a worker's relative path cannot escape the task tree by
accident. Oversized output is spilled to a file with a locator inline through a `tools/post-execute`
listener the row registers (§9's named example).

### 2.4 The workers seam (`plugins/workers/src/…`)

```rust
pub struct Workers;
impl ServiceKey for Workers { type Value = WorkersHandle; const NAME: &'static str = "workers"; }
#[derive(Clone)] pub struct WorkersHandle(Arc<WorkersInner>);
bough_util::brand_id!(pub struct WorkerId;);

#[derive(Clone, Debug)]
pub struct StartWorker {
    pub kind: WorkerKind,                       // Spawn (Phase 2) | Fork (Phase 5)
    pub spawner: AgentName, pub spawner_id: AgentId,
    pub wake: WakeId, pub step: StepId,         // the triggering step: bounds and cites need it
    pub depth: u8,
    pub task: String,
    pub seal: SealSpec,
    pub tools: Option<Restrict>,
    pub ask_mode: AskMode,
    pub at: DateTime<Utc>,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")] pub enum WorkerKind { Spawn, Fork }
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")] pub enum AskMode { Block, End }

/// The report seal (§10). Compiled once with `jsonschema`; a report that does not validate is a
/// worker failure, never a silently-accepted blob.
#[derive(Clone)]
pub struct SealSpec { pub name: String, pub schema: Arc<schemars::Schema> }
impl SealSpec { pub fn report() -> SealSpec; }        // the built-in `worker.report` seal

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub summary: String,
    /// §10: per-claim EXTERNAL cites. A claim whose only citation is this report is a THOUGHT.
    pub claims: Vec<ReportClaim>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportClaim { pub text: String, pub cites: Vec<Cite> }
impl ReportClaim { pub fn is_externally_cited(&self, worker: &WorkerId) -> bool; }

#[derive(Clone, Debug)]
pub struct WorkerResult { pub worker: WorkerId, pub outcome: WorkerOutcome, pub report: Option<Report>,
                          pub steps: u32, pub usage: Usage, pub report_step: Option<StepId> }
#[derive(Clone, Debug, PartialEq)]
pub enum WorkerOutcome { Done, Asked { question: String, message: MessageId }, Failed(String), Cancelled }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Bounds { pub max_in_flight: usize, pub max_depth: u8, pub per_wake_spawn_cap: usize }

#[async_trait::async_trait]
pub trait WorkerProvider: Send + Sync + 'static {
    fn kinds(&self) -> Vec<WorkerKind>;
    async fn start(&self, req: Arc<StartWorker>, run: WorkerRun) -> Result<WorkerResult, WorkerError>;
}
#[derive(Clone)] pub struct WorkerRun { /* id, cancel token, live status, ask channel */ }
impl WorkerRun {
    pub fn id(&self) -> &WorkerId;
    pub fn cancel(&self) -> CancellationToken;
    /// §10: surfaces as WAKE-CLASS mail on the SPAWNER's lane, and blocks or ends per `ask_mode`.
    pub async fn ask(&self, question: String) -> Result<AskAnswer, WorkerError>;
}
#[derive(Clone, Debug)] pub enum AskAnswer { Answered(String), Ended }

impl WorkersHandle {
    pub async fn provider(&self, ctx: &Context, p: Arc<dyn WorkerProvider>)
        -> Result<EffectHandle, PluginError>;
    /// Bounds are checked HERE, in the Definition, so every provider obeys the same numbers (§7).
    pub async fn start(&self, ctx: &Context, req: StartWorker) -> Result<WorkerResult, WorkerError>;
    pub fn live(&self) -> Vec<WorkerRun>;
    pub fn bounds(&self) -> Bounds;
    pub fn in_flight(&self) -> usize;
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker bound `{bound}` reached: {current} of {limit}")]
    BoundsExceeded { bound: &'static str, current: usize, limit: usize },
    #[error("no worker provider registered for kind `{0:?}`")] NoProvider(WorkerKind),
    #[error("the worker's report does not match seal `{seal}`: {detail}")]
    SealInvalid { seal: String, detail: String },
    #[error("worker `{0}` was cancelled")] Cancelled(WorkerId),
    #[error(transparent)] Agent(#[from] AgentError),
}
```

`worker-spawn` creates a **fresh task-only context** through the agent factory: an
`AgentKind::Worker` agent on its own trajectory, seeded with exactly the standing write-boundary
block + the task, `tools.restrict` applied in its scope, no projection of the spawner's history.
The boundary block is a `const` in `plugins/worker-spawn/src/boundary.rs`, not config: §7 makes it
a security invariant, and §0.2 keeps security invariants in code.

The result lands in the SPAWNER's chain: `worker/report` (Evidence, cites = the union of the
report's external cites) plus one `worker/claim` step (Thought) per uncited claim (§10).

### 2.5 The actions seam (`plugins/actions/src/…`)

```rust
pub struct Actions;
impl ServiceKey for Actions { type Value = ActionsHandle; const NAME: &'static str = "actions"; }
#[derive(Clone)] pub struct ActionsHandle(Arc<ActionsInner>);

/// §7's four sanctioned outward acts, and nothing else. A CLOSED enum: "a kind not registered by a
/// Provider does not exist" is enforced by the executor, and "a kind not in this enum" cannot be
/// spelled at all.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind { OpenPr, PushToPr, BotThreadOp, LinearWrite }
impl ActionKind { pub fn as_str(&self) -> &'static str; }

#[derive(Clone, Debug, PartialEq)]
pub struct ActionTarget { pub raw: String }
impl ActionTarget {
    /// Canonical form per kind (lowercased host, no trailing slash, `owner/repo#number`,
    /// `TEAM-123`). The idem key is a hash of THIS, so two spellings collide (§7).
    pub fn canonical(&self, kind: ActionKind) -> Result<String, ActionError>;
}

#[derive(Clone, Debug)]
pub struct ActionRequest {
    pub kind: ActionKind, pub target: ActionTarget, pub payload: serde_json::Value,
    pub agent: AgentName, pub wake: WakeId,
    /// The TRIGGERING step (§7's idem_key formula). Not the action's own step.
    pub step: StepId,
    pub at: DateTime<Utc>,
}
/// idem_key = sha256(kind ‖ canonical target ‖ triggering step id) (§7), hex.
pub fn idem_key(kind: ActionKind, canonical_target: &str, step: &StepId) -> IdemKey;

#[derive(Clone, Debug, PartialEq)]
pub struct ActionArtifact { pub locator: String, pub marker: String, pub detail: serde_json::Value }

#[async_trait::async_trait]
pub trait ActionProvider: Send + Sync + 'static {
    fn kinds(&self) -> Vec<ActionKind>;
    /// The provider embeds `req.marker` in the artifact itself (PR body, commit trailer, comment
    /// suffix) so reconciliation is a lookup against the world (§7). Phase 6 writes the providers.
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError>;
}
pub struct ExecuteRequest { pub request: Arc<ActionRequest>, pub action: ActionId,
                            pub idem_key: IdemKey, pub marker: String }

pub struct ActionsExecute;
impl WaterfallEvent for ActionsExecute {
    const NAME: &'static str = "actions/execute"; type Value = ActionExec;
}
pub struct ActionExec { pub request: Arc<ExecuteRequest>,
                        pub outcome: Option<Result<ActionArtifact, ActionError>> }

impl ActionsHandle {
    pub async fn provider(&self, ctx: &Context, p: Arc<dyn ActionProvider>)
        -> Result<EffectHandle, PluginError>;
    /// intent row + `action/intent` step BEFORE executing; `action/done` + row status after.
    /// The idem key is UNIQUE in the journal, so a concurrent duplicate collides instead of
    /// executing twice (§7).
    pub async fn execute(&self, ctx: &Context, req: ActionRequest)
        -> Result<ActionArtifact, ActionError>;
    /// Boot reconciliation: LISTS intent-without-done rows. Never re-executes (§7, §17 Phase 8).
    pub async fn pending(&self) -> Result<Vec<PendingAction>, ActionError>;
    pub fn kinds(&self) -> Vec<ActionKind>;      // exactly what some Provider registered
}
#[derive(Clone, Debug, PartialEq)]
pub struct PendingAction { pub action: ActionId, pub kind: ActionKind, pub idem_key: IdemKey,
                           pub target: String, pub marker: String, pub at: DateTime<Utc> }

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("no provider is registered for action kind `{0}`; the harness cannot perform it")]
    NoProvider(&'static str),
    #[error("action `{kind}` on `{target}` from step `{step}` is already journalled as `{action}`")]
    Duplicate { kind: &'static str, target: String, step: StepId, action: ActionId },
    #[error("`{0}` is not a valid target for `{1}`")] BadTarget(String, &'static str),
    #[error(transparent)] Provider(#[source] anyhow::Error),
    #[error(transparent)] Ledger(#[from] LedgerError),
}
```

Phase 2 registers **no** providers (§17 Phase 6), so `execute` refuses every kind with
`NoProvider`, naming it. `tool-actions` registers the four tools regardless: the refusal must be
what the model meets, and the journal must exist before the capability does.

### 2.6 New step types

Declared through `LedgerHandle::declare_step_types` by the crate that owns each (§3's
merge-extensible map, P1-D2). Bodies are `schemars`-derived structs in each crate's `vocabulary.rs`.

| type | owner | class | body |
|---|---|---|---|
| `thought/text` | `agents` | Thought | `{ text: String, step_index: u32 }` |
| `thought/reasoning` | `agents` | Thought | `{ text: String, meta: Option<Value>, step_index: u32 }` |
| `wake/jot` | `agents` | Thought | `{ of_wake: WakeId, state: String, resume_hint: String, synthetic: bool }` |
| `wake/resumed` | `agents` | Thought | `{ from_jot: StepId, of_wake: WakeId }` |
| `tool/call` | `tools` | Thought | `{ call: ToolCallId, name: ToolName, args: Value, render: RenderIntent, step_index: u32 }` |
| `tool/result` | `tools` | Either | `{ call: ToolCallId, name: ToolName, outcome: ok\|error\|denied\|blocked\|unknown, content: String, value: Option<Value>, attached: Vec<AttachedContext>, concludes_wake: bool }` |
| `worker/started` | `workers` | Thought | `{ worker: WorkerId, kind, task: String, depth: u8, seal: String }` |
| `worker/report` | `workers` | **Evidence** | `{ worker: WorkerId, summary: String, claims: Vec<ReportClaim>, steps: u32 }` |
| `worker/claim` | `workers` | Thought | `{ worker: WorkerId, text: String }` |
| `about/line` | `about-line` | **Evidence** | `{ state: String, intent: String, of_wake: WakeId }` (cites = the steps the state half summarises) |

`TOOL_OUTCOME_UNKNOWN` is `tool/result.outcome == "unknown"`: the value crash repair synthesises,
and the one outcome no live pipeline can produce.

### 2.7 Phase-1 vocabulary extensions (WP-1 owns these edits)

1. **`RequestHeader` gains three fields** (`plugins/ledger/src/vocabulary.rs`):
   `as_of: Seq` (the ledger high-water the projection was assembled at), `budget: usize`, and
   `projection_digest: String` (sha256 of `Assembled::to_text()`). Additive to a built-in body; the
   ENVELOPE is unchanged, so `LEDGER_FORMAT_VERSION` and `envelope_fingerprint()` do not move.
   These three are what make V4 a reconstruction rather than a hash comparison.
2. **`StepOutcome` gains `Restarted`** (`step/end.outcome`): the honest outcome of a step cut short
   by a joining Andrey message before its first streamed token (§2.9).
3. **`AssembleRequest` / `SectionRequest` gain `as_of: Option<Seq>`** (`plugins/projection`), and
   the assembler filters steps and rollups above it (`plugins/projection-assembler`). Without it a
   projection is only reproducible while nothing else appends; with it, re-assembly at `as_of`
   reproduces the exact bytes of any past request.
4. **`LedgerStore::action_done` takes `at: DateTime<Utc>`** (Phase 1 open item: the store read its
   own clock), and `action_intent`/`action_done` now also append their `action/intent` /
   `action/done` steps — P1-D11's deferral ends where §17 puts the seam.

### 2.8 The wake flow (`plugins/agent-loop`)

Normative: this is §5's diagram with the call sites named. `agent-loop-scripted` runs the same
sequence with steps 6–8 replaced by a transcript read.

```
 1. urgency decides the wake: Andrey message or wake-class mail ⇒ IMMEDIATE; ordinary mail ⇒ a
    debounced drain wake (`drain_debounce_ms`), unless another wake drains it first.
 2. append `wake/start { urgency, trigger, claimed }`                                  (durable)
    · an Andrey message ALWAYS opens a fresh Answer wake, whatever queue it arrived through
    · one drain wake in flight per agent; a drain wake claims ORDINARY seqs only
    · an answer wake claims its trigger only
 3. cell.claim(..) — a pure deletion splice; each claim is an `inbox/spliced { op: claim }` step
    (between steps, only `next-step` input is claimed)
 4. ctx.waterfall::<AgentPreStep>(..) — Reject, or an emptied claim, still closes a durable wake
    that spent no step (reason `completed`, consumed set carrying the claim)
 5. append `step/start { index }`                                                      (durable)
 6. projection.assemble(AssembleRequest { agent, wake, at, as_of, budget })  through
    `projection/assemble`; messages = `transcript::rebuild(steps_of_wake, as_of)` — the loop builds
    every request FROM THE LEDGER, never from in-memory state (this is what makes V4 true by
    construction rather than by discipline)
 7. append `request/header { prompt_ver, sections, tools, call, composition, as_of, budget,
    projection_digest }` ONLY when it differs from the last one in this wake              (durable)
 8. ctx.waterfall::<AgentRequest>(RequestCall { facts, call }) — call config only; the loop
    re-installs its own `facts` afterwards
 9. llm.stream(..) through `llm/stream`; every chunk appends as it is produced:
    TextDelta → `thought/text` (coalesced per flush), ReasoningDelta → `thought/reasoning`,
    ToolCall → `tool/call`, Usage → the step's `step/end` body, Failed → step 12
10. tools.execute(..) — `tools/pre-execute` → `tools/execute` → `tools/post-execute` →
    `tools/result`; each result appends `tool/result`, in MODEL order
11. append `step/end { index, outcome, detail }`                                       (durable)
12. on failure: ctx.waterfall::<AgentRequestError>(..) — `Recovery::Retry` re-enters step 8;
    `Terminal` ends the wake with reason `error`
13. tools owe another request, or next-step input arrived ⇒ step 5
14. ctx.serial::<AgentWakeStopping>(..) — every listener runs; the driver then RE-READS the inbox
    and runs another step iff fresh next-step input is there; a `concludes_wake` tool result ends
    the wake at its step
15. append `wake/end { reason, cause, consumed }`                                      (durable)
    reason ∈ completed | aborted{cause} | error | max-tokens | interrupted
16. for `completed` only: ctx.parallel::<AgentWakeEnd>(..) — the about-line refresh happens here
17. standing invariant: if unconsumed ordinary mail remains, a drain wake IS scheduled
```

Preemption (`preempt.rs`), §5's checkpoint-and-answer:

```rust
pub enum Preemption {
    /// The running wake is not an answer wake: start the answer wake NOW, concurrently, and give
    /// the interrupted wake exactly ONE grace step to jot.
    Checkpoint { answer: WakeId },
    /// An answer wake is running and has not streamed a token: the message JOINS it — the
    /// in-flight request is cancelled, `step/end { outcome: restarted }` is appended, and the same
    /// wake starts a new step with both messages claimed.
    Join { wake: WakeId },
    /// An answer wake has already streamed a token: the message queues as the next wake's first
    /// mail (`next-wake`, wake: true).
    Queue,
}
```

The grace step is a real model step with `tool_choice_none = true`, a jot instruction, and a
`grace_deadline_ms` bound; if it fails or times out the loop appends
`wake/jot { synthetic: true }` built deterministically from the wake's last thought steps, so a
continuation ALWAYS exists and never depends on a model call succeeding (P2-D14). The interrupted
wake closes with reason `interrupted`, and therefore skips step 16 — that is the mechanism behind
"a preempted wake skips its about-line refresh". The next wake of ANY kind for that agent opens
with `wake/resumed { from_jot }` and includes the jot in its first request.

Crash repair (`repair.rs`), run once at `apply` when `repair_on_boot`: for every trajectory whose
last wake has a `wake/start` and no `wake/end`, append `wake/end { reason: interrupted, consumed:
[] }`; for every `tool/call` in that wake with no matching `tool/result`, append
`tool/result { outcome: unknown }` first. It reads and writes steps only: rollups are never
touched (§5).

Per-agent scope (`scope.rs`): the loop mints `ScopeKey::new(format!("agent:{name}"))` at creation
and hands it to the handle. Shadowing is most-specific-wins (the kernel's `create_scope` already
does this); `tools.restrict` composes as an intersection.

`agent-loop` config: `{ drain_debounce_ms: u64, grace_deadline_ms: u64, default_max_tokens: i64,
prompt_ver: String, text_flush_ms: u64, repair_on_boot: bool, status_drain_ms: u64 }`.
There is no wake budget field: §5 says bounding a runaway wake is a plugin cancelling from
`agent/wake-stopping`, and inventing a `max_steps` here would be exactly the hardcoded tunable §0.2
forbids.

`agent-loop-scripted` config: `{ transcript: PathBuf | inline, strict: bool }` — a list of wakes,
each a list of steps, each step a list of `Chunk`s. It honours every waterfall and appends every
durable step, and implements neither preemption nor retry nor drain debouncing (those are
`agent-loop`'s; a replacement loop is held to the ledger protocol, not to the feature list).

### 2.9 `model-policy` and `about-line`

```rust
// model-policy: a PREPEND listener on `agent/request` (§12). Config: { sol: String, terra: String }.
// facts.answers_andrey ⇒ call.model = sol, and `model_override` is IGNORED (not overridable).
// otherwise                 call.model = facts.model_override.unwrap_or(terra)
//
// about-line: a listener on `agent/wake-end` (completed wakes only) that appends `about/line`
// citing the steps its STATE half summarises, plus a projection section at
// Position { slot: Slot::Identity, place: Place::After } rendering the newest one. The intent half
// is rendered under an explicit "intent (self-declared)" label — never as truth (§2).
```

Both `sol` and `terra` are `claude-haiku-4-5-20251001` in `bough-base` for this build (Andrey's
choice for the testing period); swapping is a one-line patch.

### 2.10 `bough exec` and the headless profile

`exec-headless` config: `{ task: String, agent: String, traj: String, print: text|json,
exit_when_idle: bool }`. On `apply` with a non-empty task it resumes-or-creates the agent, sends the
task as an Andrey message, awaits `when_idle()`, prints the last assistant text (or the whole wake
as JSON), and asks the process to exit.

Center change, minimal: `Kernel::request_exit(code: u8)` and `Kernel::exited() -> impl Future`,
domain-blind, so a surface row can end the process; `boot::boot` selects over `ctrl_c` and
`kernel.exited()`, and tears down before exit on both. Phase 3's TUI quit key needs the same
signal, so this is the earliest honest place for it.

`bough exec "<task>"` is a clap subcommand: it forces `--profile headless` and appends one
synthetic patch layer setting the `exec` row's config. No behaviour in the launcher, only
composition (§0.1 item 2).

### 2.11 Invariant modules (§0.2: every plugin crate has one)

| crate | invariant | how |
|---|---|---|
| `agents` | status never repeats; a disposed agent is terminal (no status, no wake after disposal); at most one factory | fold over `agent/status` + `agent/disposed`, per fiber, bounded |
| `agent-loop` | **the sent request reconstructs from the ledger**, byte for byte; unconsumed ordinary mail at any `wake_end` implies a scheduled drain wake; every `wake/start` has a `wake/end` or is the live one | records each request handed to the adapter (bounded, last N wakes) and compares with `reconstruct(ledger, wake)` |
| `agent-loop-scripted` | the same request-reconstruction check, imported from `agent-loop`'s pure evaluator (P2-D18) | shared pure function, two recorders |
| `llm` | every `llm/stream` ends with exactly ONE terminal chunk, and nothing follows it | wraps the stream at the seam |
| `tools` | same-step tool call/result pairing (§0.2's example); no `tool/result` without a `tool/call` in the same wake and step | fold over `ledger/step` |
| `workers` | live runs never exceed `max_in_flight`; no run exceeds `max_depth`; every `worker/report` has a `worker/started` | fold over the run registry + `ledger/step` |
| `actions` | intent-before-done on every journal row; no two rows share an idem key | reads the `actions` table at quiesce |
| `about-line` | every `about/line` cites at least one step that exists, and follows a `completed` `wake/end` | fold over `ledger/step` |
| `model-policy` | an answer wake's request never carries `terra`, and `model_override` never appears on an answer wake | records `agent/request` outcomes |
| `llm-anthropic`, `llm-replay`, `llm-retry`, `tools-baseline`, `tool-*`, `worker-spawn`, `exec-headless` | `No runtime invariant:` with the reason (mappers and consumers; the seam's own invariant covers their output) | — |

### 2.12 Bundle rows

`bundles/bough-base.yml` gains, in reading order (row order carries no load semantics, §0.2):

```yaml
- { id: llm,             plugin: llm }
- { id: llm.anthropic,   plugin: llm-anthropic, config: { models: "*", api_key_env: ANTHROPIC_API_KEY, request_timeout_ms: 120000 } }
- { id: llm.retry,       plugin: llm-retry,     config: { max_attempts: 4, min_delay_ms: 250, max_delay_ms: 8000, jitter: true, retry_on: [transport, rate_limit, overloaded] } }
- { id: tools,           plugin: tools,         config: { default_deadline_ms: 120000, max_parallel: 8 } }
- { id: tools.baseline,  plugin: tools-baseline, config: { root: !!expr 'cwd()', bash_timeout_ms: 120000, max_output_bytes: 20000, max_read_bytes: 400000, deny_globs: [] } }
- { id: agents,          plugin: agents }
- { id: agent.loop,      plugin: agent-loop,    config: { drain_debounce_ms: 120000, grace_deadline_ms: 20000, default_max_tokens: 8192, prompt_ver: "p2.1", text_flush_ms: 400, repair_on_boot: true, status_drain_ms: 500 } }
- { id: model.policy,    plugin: model-policy,  config: { sol: claude-haiku-4-5-20251001, terra: claude-haiku-4-5-20251001 } }
- { id: about.line,      plugin: about-line,    config: { max_state_chars: 400, max_intent_chars: 200 } }
- { id: workers,         plugin: workers,       config: { max_in_flight: 8, max_depth: 3, per_wake_spawn_cap: 4 } }
- { id: worker.spawn,    plugin: worker-spawn,  config: { ask_mode: block, max_steps: 40 } }
- { id: tool.spawn_worker, plugin: tool-spawn_worker }
- { id: tool.ask,        plugin: tool-ask }
- { id: actions,         plugin: actions }
- { id: tool.actions,    plugin: tool-actions }
```

`bundles/bough-headless.yml`: `- { id: exec, plugin: exec, config: { task: "", agent: sol, traj: "lane/sol", print: text, exit_when_idle: true } }`.
`agent-loop-scripted` and `llm-replay` are in the catalog and in **no** bundle; the swap patches
name them, exactly as `ledger-memory` is handled.

---

## Work packages

Eight packages, file sets disjoint. The one shared file is the root `Cargo.toml`: each package
appends its own `bough-plugin-*` line to `[workspace.dependencies]` and its own member glob is
already covered by `plugins/*`. `crates/bough/Cargo.toml` (which links the catalog) is WP-8's
alone, so every package before it tests with `Catalog::from_parts(..)` rather than the full binary.

Order: WP-1 first (everything compiles against its vocabulary), then WP-2 and WP-3 in parallel,
then WP-4, then WP-5/6/7 in parallel, then WP-8.

### WP-1: the model seam, and the Phase-1 vocabulary extensions

Files: `plugins/llm/**`, `plugins/llm-anthropic/**`, `plugins/llm-replay/**`,
`plugins/llm-retry/**`, `plugins/ledger/src/vocabulary.rs`, `plugins/ledger/src/lib.rs`,
`plugins/ledger-sqlite/src/read.rs`, `plugins/ledger-memory/src/lib.rs`,
`plugins/projection/src/lib.rs`, `plugins/projection/src/section.rs`,
`plugins/projection-assembler/src/assemble.rs`, `plugins/projection-assembler/src/resolve.rs`,
root `Cargo.toml` (four lines).

§2.1 and §2.7 in full: the chunk vocabulary, the adapter registry with an explicit
`resolve(model) -> adapter`, the `agent/request` / `agent/request-error` / `llm/stream` waterfalls,
the three provider crates, and the four Phase-1 edits (`RequestHeader` +3 fields,
`StepOutcome::Restarted`, `as_of` on assemble/section requests, `action_done(at)` plus the two
action steps). The `as_of` filter must apply to the built-in bands AND be handed to contributed
sections, or reconstruction is only as good as the sections nobody contributed.

Tests it must ship: `llm/tests/stream.rs` — a failure is a terminal chunk and never an `Err`; a
stream carries exactly one terminal chunk; a `llm/stream` listener that returns without `next()`
and without filling `stream` yields a `Failed` chunk rather than a hang; adapter resolution picks
Exact over Prefix over Any and reports a tie. `llm-anthropic/tests/map.rs` — a canned
`LlmClient` (bough-llm's `test_support`) maps text, reasoning, tool calls and usage onto the chunk
vocabulary in order; an absent API key is `Failed { Auth }`, not a panic; `#[ignore]` live:
`a_live_haiku_round_streams_text_tool_calls_and_usage`. `llm-replay/tests/replay.rs` —
determinism across two runs; an unmatched request fails in strict mode. `llm-retry/tests/retry.rs` —
a retryable failure returns `Retry` without calling `next()`; a non-retryable one delegates and
stays terminal; attempts are bounded. Projection: `as_of` reproduces a past assembly byte for byte
while later steps exist (`projection-assembler/tests/golden.rs` gains one case, both providers).
Ledger: `RequestHeader`'s new fields round-trip and the envelope fingerprint does NOT move;
`action_done` no longer reads a clock and both providers append `action/intent` + `action/done`.

### WP-2: the agents seam

Files: `plugins/agents/**`, root `Cargo.toml` (one line).

§2.2 in full: ids, `Session`, `Agent`, `Inbox` with the durable splice, the three presets, typed
cancellation, the creation transaction with rollback, `AgentDisposer` as a capability, the factory
slot, the live registry, the initiator task-local, the twelve `agent/*` events with their dispatch
modes, the four new step types it owns, and `src/invariant.rs`.

Tests: `lifecycle.rs` — first cause wins across two concurrent cancels; a cancel with nothing
active is a no-op AND does not arm the next wake; a `Disposed` cancel never latches a pending wake;
a `setup()` that returns `Err` leaves no session, no registry entry, no scope and no steps beyond
what was already durable; teardown runs stop+drain → scope → agent → session, asserted by an order
trace; the disposer is the only path to teardown. `inbox.rs` — every mutation appends
`inbox/spliced` keyed by the message id; insert/claim/discard fold back to the live inbox
(`Inbox::rebuild`); the three presets map to the documented (target, wake) pairs. `factory.rs` —
`set_factory` twice is an error naming the first driver; unloading the driver row frees the slot.
`invariant.rs::tests` — a planted repeated status is reported; a status after disposal is reported;
two fibers are two streams (Phase 1's lesson).

### WP-3: the tools seam and the baseline tool set

Files: `plugins/tools/**`, `plugins/tools-baseline/**`, root `Cargo.toml` (two lines).

§2.3 in full: the scoped registry with most-specific-wins shadowing, `Restrict` as an intersection,
`schemas()` as the single source of what the prompt shows, the monotone `Decision` (no public
widening constructor), the three waterfalls plus `tools/result`, the concurrency-safe/barrier
dispatcher with model-ordered results, deadline wrapping, the `approval` optional key, the two step
types it owns, and `src/invariant.rs` (same-step call/result pairing). Then the six baseline tools
with their render intents and the oversized-output spill listener.

Tests: `pipeline.rs` — the nine cases of V5 (deny is sticky across a later listener; ask degrades
to deny with no `approval`; ask is serviced when a stub approver is mounted; accept replaces
content OR value, never both; attach adds contexts without touching either; block yields a
valueless failure with `FailureClass::Blocked`; two concurrency-safe calls overlap in time; an
unsafe call between them forms a barrier; results come back in model order regardless).
`restrict.rs` — a restricted tool is absent from `schemas()` and is refused by `resolve()` with the
same `NotFound` a nonexistent name gets, message included. `scope.rs` — an agent-scoped tool
shadows its global twin for that agent only. `tools-baseline/tests/tools.rs` — each of the six on a
tempdir: read/write/edit round-trip, glob and grep results, bash exit codes and timeout, a spill
that leaves a locator inline, and a path outside `root` refused.

### WP-4: `agent-loop` — the wake flow

Files: `plugins/agent-loop/**`, root `Cargo.toml` (one line).

§2.8 in full: `wake.rs` (the seventeen-step flow), `mail.rs` (urgency, claim selectors, the
consumed-set union, drain scheduling and the one-in-flight rule, the standing invariant),
`transcript.rs` (the pure ledger→messages fold both the loop and the invariant use),
`request.rs` (assemble → `LlmRequest` → `request/header` on change only), `preempt.rs`,
`repair.rs`, `scope.rs`, `driver.rs` (the `AgentFactory`/`AgentDriver` impls), `invariant.rs`.

Tests: `mail.rs` — V6's six cases. `preemption.rs` — V2's five cases. `repair.rs` — V9's three
cases. `reconstruct.rs` — every request of a multi-step wake rebuilds byte for byte from the ledger;
a planted side-channel message (a `llm/stream` listener that appends to `messages`) makes the
invariant report. `flow.rs` — a rejected `agent/pre-step` still closes a durable wake that spent no
step; claimed messages a decision omits stay removed; `request/header` is appended only when it
changes; a `concludes_wake` tool result ends the wake at its step; a `wake-stopping` listener that
steers runs another step and listener ORDER does not change the outcome; a plugin failure ends the
wake and not the loop.

### WP-5: `agent-loop-scripted`, `model-policy`, `about-line`

Files: `plugins/agent-loop-scripted/**`, `plugins/model-policy/**`, `plugins/about-line/**`,
root `Cargo.toml` (three lines).

The second loop provider (transcript-driven, honouring every waterfall and every durable step, and
importing `agent-loop`'s pure reconstruction evaluator for its own invariant); the prepend policy
listener; the about-line listener + its `about/line` step type + its Identity/After section.

Tests: `agent-loop-scripted/tests/replay.rs` — a two-wake transcript appends `wake/start`,
`inbox/spliced`, `step/start`, `request/header`, thought steps, `step/end`, `wake/end` in §5's
order, with the consumed set on `wake/end`; the same transcript twice is byte-identical.
`model-policy/tests/policy.rs` — V6's three policy cases plus "sol is not overridable".
`about-line/tests/about.rs` — a completed wake refreshes the line, the state half cites the steps it
summarises, the intent half renders under its label, an interrupted wake refreshes nothing, and the
section appears at Identity/After in the projection.

### WP-6: workers

Files: `plugins/workers/**`, `plugins/worker-spawn/**`, `plugins/tool-workers/**`,
root `Cargo.toml` (three lines).

§2.4 in full: the Definition (start/result vocabulary, live runs, the three bounds enforced in the
Definition, the provider registry, the three step types), `worker-spawn` (fresh task-only agent
through the factory, the boundary block prepended by the SPAWNER, seal compiled with `jsonschema`,
report → cited evidence + thoughts, `ask()` as wake-class mail on the spawner's lane with
block/end modes), and the two Consumer rows `tool-spawn_worker` / `tool-ask`.

Tests: `workers/tests/bounds.rs` — each of the three bounds refuses the excess with
`BoundsExceeded` naming the bound, and the per-wake cap resets at the next wake.
`worker-spawn/tests/roundtrip.rs` — the request the adapter receives begins with the boundary block
(asserted on the recorded `LlmRequest`, not on the prose that asked for it); a scripted worker's
report validates against the seal; an invalid report is `SealInvalid`; the result lands in the
SPAWNER's chain as `worker/report` with the external cites; a claim citing only the worker's own
report lands as `worker/claim` (Thought); `ask()` appears as wake-class mail on the spawner's inbox
and blocks (mode `block`) or ends the worker (mode `end`).
`worker-spawn/tests/live_task.rs` — `#[ignore]`, `BOUGH_LIVE=1`: a real haiku worker edits a file
in a tempdir and reports it, and the file's content proves it.

### WP-7: actions

Files: `plugins/actions/**`, `plugins/tool-actions/**`, root `Cargo.toml` (two lines).

§2.5 in full: the closed four-kind enum, target canonicalisation, `idem_key`, the journal writes
(intent row + `action/intent` step BEFORE, `action/done` after), the `actions/execute` waterfall,
the executor's refusal of an unregistered kind, `pending()` reconciliation, and `src/invariant.rs`.
Then `tool-actions`: four tools whose schemas describe the primitives, each calling
`ActionsHandle::execute` — so the natural path for a model IS the journaled one.

Tests: `journal.rs` — V7's four cases, plus: the marker embedded in `ExecuteRequest` is derived
from the idem key; a provider `Err` marks the row `failed` and still writes `action/done`; the
canonicaliser collapses two spellings of one target so they collide.
`tool-actions/tests/refusal.rs` — with no provider mounted, each of the four tools returns a
`ToolResult` whose failure names the kind, and a fifth spelling is not a tool at all.

### WP-8: integration — `bough exec`, bundles, swap, the phase gates

Files: `plugins/exec-headless/**`, `crates/bough-kernel/src/kernel.rs`,
`crates/bough/src/{cli.rs,boot.rs,compose.rs,exec.rs,lib.rs}`, `crates/bough/Cargo.toml`,
`crates/bough/tests/{agent_scripted.rs,loop_swap.rs,exec_headless.rs,agent_invariants.rs,fixtures/*.yml,support/mod.rs}`,
`bundles/bough-base.yml`, `bundles/bough-headless.yml`, `Makefile`, `BUILD.md`, root `Cargo.toml`.

The exec row, the kernel exit signal, the `exec` subcommand, the fifteen new base rows, the two swap
patch fixtures (`agent-loop` → `agent-loop-scripted`; `llm-anthropic` → `llm-replay`), a `make live`
target that runs the `#[ignore]`d live set with the key sourced from `~/.bough/env`, and the phase's
own gates.

Tests: `agent_scripted.rs` (V1), `loop_swap.rs` (SWAP), `exec_headless.rs` (V9's second half,
offline and `#[ignore]`d live), `agent_invariants.rs` — the ledger, agents, agent-loop, tools,
workers and actions invariants all report clean over a scripted session, and one planted violation
of each of the three new ones is reported through the runner. Plus a wake-latency measurement
(`#[ignore]`, `BOUGH_BENCH=1`) that times inbox-receipt → first adapter request, which is the
number that decides Phase 0's open item 1 (below).

---

## 4. Verification map

Every bullet of §17 Phase 2 and of the phase brief, against the test that proves it. A bullet with
no green named test is not done.

This map was REWRITTEN at the review close: the names below are the names in the tree, checked
against `fn` names, and where a bullet's claim was weaker than its wording the wording was
corrected rather than the claim inflated.

**V1 — a scripted multi-wake conversation.** `crates/bough/tests/agent_scripted.rs::`
`a_scripted_conversation_appends_every_durable_step_in_order`,
`wake_end_carries_the_reason_and_the_consumed_seq_set`,
`the_ledger_and_agents_invariants_hold_across_the_conversation`,
each also as `…_under_the_scripted_driver` (6 green).
DEVIATION, stated: the first asserts pairwise ORDERING RELATIONS between kinds, not one exact kind
sequence, and its conversation makes no tool call — `tool/call` before `tool/result` in a real wake
is proven by `plugins/agent-loop/tests/flow.rs::durable_tool_results_stay_model_ordered_in_the_ledger`
and by the `tools` invariant, not here.

**V2 — preemption mid-thought.** `plugins/agent-loop/tests/preemption.rs::`
`an_andrey_message_starts_its_answer_wake_immediately`,
`the_interrupted_wake_gets_exactly_one_grace_step_to_jot`,
`a_failed_grace_step_still_leaves_a_synthetic_jot`,
`the_next_wake_of_any_kind_resumes_from_the_jot`,
`a_message_before_the_first_token_joins_and_after_it_queues`,
`a_message_before_the_first_token_joins_the_answer_wake`,
`a_message_after_the_first_token_queues_as_next_wake_mail`,
`a_message_during_an_answer_wake_does_not_open_a_second_one`,
`a_preempted_wake_skips_its_about_line_refresh`,
`an_interrupt_reaches_a_tool_that_is_already_running` (NEW at the review close),
`when_idle_does_not_return_while_a_second_wake_is_still_open` (NEW at the review close).
DEVIATION: `a_preempted_wake_skips_its_about_line_refresh` does not MOUNT the `about-line` row; it
registers `bough_plugin_about_line::refresh` on the same moment by hand, so it proves the
reason-guard rather than the registration. The real row over both drivers is exercised by
`crates/bough/tests/loop_swap.rs::about_line_tools_workers_and_model_policy_keep_working_against_the_{live,scripted}_driver`.

**V3 — a worker roundtrip on a real small task.** `plugins/worker-spawn/tests/roundtrip.rs::`
`the_seeded_task_begins_with_the_boundary_block`,
`a_scripted_workers_report_validates_against_the_seal`,
`an_invalid_report_is_seal_invalid_naming_the_seal_and_the_pointer`,
`the_report_lands_in_the_spawners_chain_with_the_external_cites`,
`a_claim_citing_only_the_workers_own_report_lands_as_a_thought`,
`ask_appears_as_wake_class_mail_on_the_spawners_lane`,
`ask_in_block_mode_waits_for_the_answer`,
`ask_in_end_mode_ends_the_worker_without_waiting`,
`a_block_mode_ask_with_no_answer_ends_the_worker`;
over a whole mounted tree: `crates/bough/tests/worker_spawn.rs::`
`the_boundary_block_is_first_in_the_request_the_adapter_receives`,
`the_worker_context_is_task_only`,
`a_workers_question_lands_on_the_spawners_lane_as_a_durable_wake_class_splice`;
bounds: `plugins/workers/tests/bounds.rs::`
`max_depth_refuses_the_generation_past_the_limit_and_names_it`,
`max_in_flight_refuses_the_third_concurrent_run_and_names_it`,
`the_per_wake_cap_refuses_the_third_spawn_of_one_wake_and_names_it`,
`the_per_wake_cap_resets_at_the_next_wake`,
`a_kind_with_no_provider_is_refused_and_reserves_nothing`,
`a_refused_start_does_not_spend_the_per_wake_budget`;
live: `crates/bough/tests/worker_live.rs::a_real_worker_edits_a_file_and_its_content_proves_it`
(`BOUGH_LIVE=1`, `claude-haiku-4-5-20251001`).

**V4 — model-visible ⟺ ledgered.** `plugins/agent-loop/tests/reconstruct.rs::`
`every_request_of_a_wake_reconstructs_byte_for_byte`,
`a_side_channel_message_makes_the_invariant_report`,
`a_contributed_section_added_mid_wake_does_not_break_a_past_reconstruction`,
`the_grace_step_is_ledgered_and_runs_the_agent_request_waterfall` (NEW at the review close);
the evaluator itself: `plugins/agent-loop/src/invariant.rs::tests::`
`a_matching_pair_is_clean`, `a_digest_mismatch_is_a_violation`,
`a_side_channel_message_is_a_violation`,
`a_step_with_no_header_at_or_before_it_is_a_violation`,
`a_header_from_an_earlier_step_still_anchors_a_later_one`;
through the runner: `crates/bough/tests/agent_invariants.rs::a_planted_side_channel_is_reported`,
`::every_invariant_reports_clean_over_a_scripted_session`.
The system half is now TOTAL: the projection digest is part of what makes a `request/header`
change, so every step has an anchoring header at or before it and a step with none is reported.

**V5 — the tools pipeline.** `plugins/tools/tests/pipeline.rs::`
`a_denial_cannot_be_re_allowed_by_a_later_listener`,
`ask_degrades_to_deny_without_approval`,
`ask_is_serviced_when_approval_is_mounted`,
`accept_replaces_content_or_value_never_both`,
`accept_may_attach_contexts`,
`block_yields_a_valueless_failure`,
`concurrency_safe_calls_dispatch_in_parallel`,
`an_unsafe_call_forms_an_exclusive_barrier`,
`durable_results_stay_model_ordered`,
`a_tool_outside_the_scope_is_refused_by_the_executor`;
`plugins/tools/tests/restrict.rs::`
`a_restricted_tool_is_absent_from_the_schema`,
`a_restricted_tool_is_refused_indistinguishably_from_a_nonexistent_one`,
`two_restrictions_compose_as_an_intersection`,
`disposing_a_restriction_restores_visibility`;
in the ledger: `plugins/agent-loop/tests/flow.rs::durable_tool_results_stay_model_ordered_in_the_ledger`;
the crate's own invariant: `plugins/tools/src/invariant.rs::tests` (7, including
`a_wake_that_closes_over_an_unanswered_call_is_a_violation` and
`a_result_recorded_without_a_step_index_field_is_a_step_mismatch`).

**V6 — mail consumption and wake urgency.** `plugins/agent-loop/tests/mail.rs::`
`consumed_is_the_union_of_wake_end_sets`,
`concurrent_wakes_over_disjoint_seqs_never_regress_consumption`,
`unconsumed_ordinary_mail_implies_a_scheduled_drain_wake`,
`only_one_drain_wake_is_in_flight_per_agent`,
`an_andrey_message_gets_a_fresh_answer_wake_from_either_queue`,
`a_drain_wake_never_answers_andrey`;
`plugins/model-policy/tests/policy.rs::{an_answer_wake_gets_sol, an_unattended_wake_gets_terra,
model_override_applies_to_unattended_only, sol_is_not_overridable}`;
the policy invariant is no longer self-confirming — it joins the DECISION to the model the durable
`request/header` records: `plugins/model-policy/src/invariant.rs::tests::`
`a_later_listener_rewriting_the_model_is_a_violation`, `a_matching_decision_and_header_are_clean`.

**V7 — the actions journal.** `plugins/actions/tests/journal.rs::`
`intent_is_written_before_execute_and_done_after`,
`the_same_kind_target_and_step_collide_instead_of_duplicating`,
`an_unregistered_kind_is_refused_by_the_executor`,
`reconciliation_lists_intent_without_done_without_re_executing`,
`two_spellings_of_one_target_produce_one_idem_key`,
`the_marker_the_provider_is_handed_is_derived_from_the_idem_key`,
`a_provider_failure_marks_the_row_failed_and_still_writes_a_done`;
`plugins/tool-actions/tests/refusal.rs::{each_primitive_refuses_with_no_provider_mounted,
a_fifth_spelling_is_not_a_tool_at_all}`.

**V8 — cancellation and lifecycle.** `plugins/agents/tests/lifecycle.rs::`
`the_first_cancel_cause_wins` (a REAL race since the review close: two spawned tasks on a
multi-threaded runtime behind a barrier, not `tokio::join!`),
`a_cancel_with_nothing_active_is_a_no_op_and_arms_nothing`,
`a_disposed_cancel_never_latches_a_pending_wake`,
`a_setup_failure_rolls_the_creation_back_fully`,
`teardown_order_is_stop_then_scope_then_agent_then_session`,
`the_disposer_is_the_only_path_to_teardown`;
`plugins/agents/src/invariant.rs::tests::{a_repeated_status_is_reported,
a_status_after_disposal_is_reported, two_fibers_are_two_streams,
forget_drops_only_that_fibers_observations}`;
through the runner: `crates/bough/tests/agent_invariants.rs::a_planted_status_repeat_is_reported`.

**V9 — crash repair and `bough exec`.** `plugins/agent-loop/tests/repair.rs::`
`an_orphaned_trailing_wake_closes_as_interrupted`,
`a_call_without_a_result_gets_tool_outcome_unknown`,
`a_crash_during_a_preemption_closes_both_open_wakes` (NEW: checkpoint-and-answer leaves TWO wakes
open, and repair now closes every one, not only the trailing one);
`crates/bough/tests/exec_headless.rs::`
`repair_at_boot::booting_exec_closes_an_orphaned_wake_and_leaves_rollups_alone` — this, and NOT
`repair.rs::repair_never_touches_rollups`, is the evidence for the rollup half: it seals a rollup,
boots `bough exec` and re-reads it, whereas the planner test inspects a struct with no rollup field;
`::exec_runs_one_task_end_to_end_with_llm_replay`, `::exec_exits_with_the_ledger_intact`,
`::exec_tears_down_before_exit`, `::an_empty_task_is_not_a_task_and_the_row_still_activates`,
`::exec_runs_one_task_live_with_haiku` (`BOUGH_LIVE=1`).

**V10 — the llm seam.** `plugins/llm/tests/stream.rs::{a_failure_is_a_terminal_chunk_never_an_error,
every_stream_ends_with_exactly_one_terminal_chunk, a_short_circuiting_wrapper_yields_a_failed_chunk}`;
`plugins/llm-retry/tests/retry.rs::{a_retryable_failure_is_retried_without_next,
the_default_leaves_the_failure_terminal_for_the_wake, attempts_are_bounded,
the_delay_stays_inside_the_configured_window}` — and the bound is a bound as INTEGRATED since the
review close: `agent-loop` carries the attempt count across the retry `continue` instead of
rebuilding `attempt: 1` per step;
`plugins/llm-anthropic/tests/map.rs::{text_reasoning_tool_calls_and_usage_map_to_the_seam,
an_absent_key_is_a_terminal_auth_failure, a_transport_failure_is_terminal_and_retryable,
the_adapter_does_not_retry_on_its_own, the_call_config_is_what_reaches_the_client}`
and `::a_live_haiku_round_streams_text_tool_calls_and_usage` (`BOUGH_LIVE=1`);
`plugins/llm-replay/tests/replay.rs::{the_same_transcript_answers_deterministically,
an_unmatched_request_fails_in_strict_mode, a_lenient_replay_ends_the_turn_instead_of_hanging,
a_round_without_a_terminal_chunk_is_closed_by_the_adapter}`.

**SWAP — the phase's exit gate.** `crates/bough/tests/loop_swap.rs::`
`a_patch_mounts_agent_loop_scripted_in_place_of_agent_loop_without_a_recompile`,
`about_line_tools_workers_and_model_policy_keep_working_against_the_live_driver`,
`about_line_tools_workers_and_model_policy_keep_working_against_the_scripted_driver`,
`the_ledger_and_agents_invariants_run_against_both_loop_providers`,
`the_retired_loop_leaves_no_factory_no_listeners_and_no_bindings` (all THREE halves asserted since
the review close: the factory slot, the listener counts on the four `agent/*` moments, the
per-round `llm/stream` hop, and the row's own bindings),
`a_patch_replaces_llm_anthropic_with_llm_replay`.
`agent-loop-scripted` runs a scripted `tool/call` through the REAL guarded pipeline since the
review close, so "tools keep working unchanged" is a functional claim and not an activation one.

---

## 5. What Phase 2 does NOT build

Stated so a reviewer does not look for it: no mail ROUTER (§5's routing rules are Phase 5; here a
message is addressed to an agent by its sender), no dormancy, no ticks, no schedules (`ctx.schedule`
is Phase 6), no collectors, no MCP, no action PROVIDERS (Phase 6 — every kind is refused), no
`worker-fork` and no `tool-fork` (Phase 5, with graph ops), no rollups or digests (Phase 4 — the
projection runs with zero rollups, as in Phase 1), no leader, no TUI, no `ctx.commands`, no
approval provider (so `ask` degrades to deny, by design), no wards. Multiple residents are possible
by construction but only one is exercised: §17 puts "many agents" in Phase 5.

---

## 6. Decisions taken where REQUIREMENTS is silent

- **P2-D1 — a Service Definition that owns STATE registers a catalog row.** P1-D1 kept `ledger` and
  `projection` row-less because they owned only vocabulary. `agents`, `tools`, `workers`, `actions`
  and `llm` own live state (the registry, the tool map, the run table, the journal handle, the
  adapter map) and must be the thing that `provide`s their key, so each is a row. *Alternative:* let
  a Provider own the registry, rejected because then two Providers would mean two registries and
  the seam would stop being a seam.
- **P2-D2 — the seam's message vocabulary IS `bough-llm`'s, re-exported.** §12 says the llm
  Definition owns the message vocabulary and §13 says do not redesign bough-llm. Declaring a second
  `Message`/`Block` pair would put a lossy mapper on the byte-for-byte path V4 depends on.
  The seam adds only what bough-llm lacks: the streaming chunk vocabulary and the adapter registry.
- **P2-D3 — `CallConfig`, `RequestFacts` and the `agent/request(-error)` types live in
  `plugins/llm`, re-exported from `plugins/agents`.** §12 puts those waterfalls in the llm
  Definition; §2 puts the `agent/*` names in agents. Types in llm + a re-export satisfies both and
  keeps the dependency acyclic (agents → llm, never back).
- **P2-D4 — `agent/request` carries `Arc<RequestFacts>` and the loop re-installs its own copy after
  the waterfall.** §5 says the waterfall is over the call config only and cannot mutate messages;
  a listener still needs to know whether the wake answers Andrey. Read-only facts + a post-chain
  re-install makes "cannot mutate" true rather than documented.
- **P2-D5 — retries live in `llm-retry` only; `llm-anthropic` disables bough-llm's own.** §12 makes
  retry a waterfall listener, not adapter code. Two retry layers would multiply attempts and make
  the `agent/request-error` attempt counter a lie.
- **P2-D6 — text deltas stream live; tool calls, reasoning and usage arrive at the round's end.**
  That is the whole of `LlmClient`'s surface (`run` + `on_text`). Streaming tool-call deltas would
  mean editing bough-llm, which §13 forbids for this phase. The seam's vocabulary already allows
  incremental tool calls, so a later bough-llm change is additive.
- **P2-D7 — an absent API key is a call-time terminal chunk, not a boot failure.** "Misconfiguration
  fails loud" (§0.2) is about COMPOSITION; a credential is runtime state, and failing the boot would
  make every offline test host unable to mount `bough-base`.
- **P2-D8 — the live inbox is a cache of the `inbox/spliced` fold.** §2 makes every mutation durable
  but does not say which copy is authoritative. The fold is; `Inbox::rebuild` is the same function
  used at resume and by crash repair, so the two can never drift.
- **P2-D9 — `set_status` REFUSES a repeat rather than letting the invariant report it.** §0.2 lists
  non-repeating status as the agents invariant; a rule that is only observed is a rule that ships
  broken between two test runs. The invariant still runs (a driver could publish through another
  path in a later phase).
- **P2-D10 — `agent/wake-stopping` is `SerialEvent` with `Output = Infallible`.** §5 says serial AND
  says data decides so listener order cannot change the outcome. First-return-wins would make order
  decisive; an uninhabited output means every listener runs, in order, and the decision is read from
  the inbox afterwards.
- **P2-D11 — the about-line refresh is its own `about/line` step, appended right after `wake/end`
  and citing it, dispatched by a new `agent/wake-end` PARALLEL event for completed wakes only.**
  §5 says `wake/end` "carries the about-line refresh" and §2 says the refresh is a listener's work
  stored as a step. A plugin writing into another plugin's step body would break the ledger's
  ownership rule (§3), so the moment is shared, not the row.
- **P2-D12 — the pre-execute guard is monotone BY TYPE.** `Decision` has no public widening
  constructor: listeners can only call `deny()` or `ask()`, and `Allow` is the executor's initial
  value. §9 demands monotonicity; a runtime clamp would need per-hop interception the kernel does
  not offer.
- **P2-D13 — `tools/execute` wrappers are checked, not trusted.** After the chain the executor
  compares the call's digest and ignores (and logs) any edit, and takes `min` of the deadlines. §9
  says wrappers may replace only the cancellation signal; nothing in a waterfall enforces that.
- **P2-D14 — a jot always exists.** If the grace step fails or times out, the loop appends
  `wake/jot { synthetic: true }` built from the wake's last thought steps. §5 promises the next wake
  can resume; making that depend on a model call succeeding would make the promise conditional.
- **P2-D15 — a joining message cancels the in-flight request and restarts the step
  (`step/end.outcome = restarted`).** §5's cutoff is "the first reply token has streamed", which is
  exactly the point before which restarting costs nothing. Splicing into the next step boundary
  would satisfy the letter and lose the latency the rule exists for.
- **P2-D16 — wake urgency and drain scheduling live in `agent-loop`, not in a `wake-scheduler` row.**
  §5 names `wake-scheduler` and `preemption` as plugins, but both are pure loop mechanics with no
  second implementation in sight, and §0.2 forbids splitting preemptively. They are modules
  (`mail.rs`, `preempt.rs`) until Phase 7's ticks give them a second caller. Flagged for the
  §15-item-6 review at phase close.
- **P2-D17 — `mail-router` is not in this phase at all.** §5 lists it; §17 Phase 5 is where routing
  rules and multiple lanes arrive. Here a message names its recipient.
- **P2-D18 — the request-reconstruction evaluator is a pure function in `agent-loop`, imported by
  `agent-loop-scripted`.** §2 requires the ledger and agents invariants to run against every loop
  provider. Copying the evaluator would let the copies drift; a shared pure function with two
  recorders cannot.
- **P2-D19 — the loop builds every request FROM THE LEDGER** (`transcript::rebuild` over the wake's
  own steps), not from an in-memory conversation. It costs a read per step and it is what makes
  "model-visible ⟺ ledgered" true by construction rather than by discipline.
- **P2-D20 — `request/header` gains `as_of`, `budget` and `projection_digest`.** Without a
  reconstruction anchor V4 can only compare hashes, and §0.2's invariant is stated as
  reconstructibility. The envelope is unchanged, so the ledger format version does not move.
- **P2-D21 — the write-boundary block is a `const` in `worker-spawn`, not config.** §0.2 keeps
  security invariants in code and deployment-varying values in config; §7 makes the boundary the
  former. A patch can disable the row, which is Andrey's act, and cannot edit the text.
- **P2-D22 — `ActionKind` is a closed enum.** §7 says the four are harness primitives and the
  executor is where the set is enforced. A string kind would make "Slack send is not a kind" a
  runtime lookup instead of a compile-time fact, and Phase 6's proof is much weaker for it.
- **P2-D23 — the kernel gains `request_exit` / `exited`.** A row must be able to end the process
  (`bough exec`, and Phase 3's quit key). It carries no domain vocabulary, and the launcher still
  owns the exit path and the teardown, so §0.1 holds.
- **P2-D24 — Phase 0's poll-loop deferral is decided by MEASUREMENT, not by fiat.** WP-8 ships
  `wake_latency_from_receipt_to_first_request` (`#[ignore]`, `BOUGH_BENCH=1`). The 20ms/5ms/1ms
  poll only costs latency on fiber transitions, and a wake is not a fiber transition, so the
  expectation is that it does not show up; if the measurement says otherwise, the notify rewrite
  lands in this phase and this decision is rewritten with the number. Either way the number is
  recorded in `BUILD.md`.
- **P2-D25 — nothing durable rides an `emit`.** Phase 0 left `emit` spawned and unawaited, so every
  `agent/*` emit event in §2.2 is a LIVE mirror of a step that is already committed, and no code
  path in this phase reads an emit to decide anything. Tests that observe live events poll or
  quiesce, exactly as Phase 1's do.
- **P2-D26 — `tool/result.class` is `Either`.** A `read_file` result citing a path is evidence; a
  `bash` result usually is not. The tool decides by supplying cites, and the ledger's
  evidence-requires-cites rule does the rest.
- **P2-D27 — live tests are `#[ignore]`d and gated on `BOUGH_LIVE=1`, and `make gates` stays
  offline.** Andrey asked for real haiku testing, and `make live` is the target that does it
  (sourcing `~/.bough/env`); a hermetic default suite is AGENTS.md's rule and the reason the whole
  offline path runs on `llm-replay` and `agent-loop-scripted`.

---

## 7. Deviations and open items (written at the review close, 2026-08-26)

### 7.1 What the review changed

Every HIGH and MEDIUM finding is fixed, each with a named test that ran green. The ones that
changed a shape rather than a line:

- **`request/header` now compares the projection digest too.** §5 lists four things ("prompt
  version, section ids, tool schemas, call config"); the header also anchors V4's reconstruction of
  the SYSTEM prefix, and a step whose prefix moved with no header for it is a prefix nothing in the
  ledger describes. The digest is therefore part of what makes a header change, and the V4 check
  demands an anchor at or before EVERY step instead of skipping steps that had none. Cost: a
  tool-using wake writes one header per step. `flow.rs::a_request_header_is_appended_only_when_it_changes`
  states the new rule (no two consecutive headers are equal, and §5's four are unchanged across
  the two steps — only the digest moved). "Tool schemas" is a DIGEST of the definitions, not the
  name list, so a scoped tool shadowing its same-named global twin changes the header.
- **The grace step is a real step.** `wake/grace-prompt` (a new `agents` step type),
  `step/start`, `request/header`, `step/end`, the `agent/request` waterfall, and the recorded
  `SentRequest`. It previously built its own `LlmRequest` with `model: ""` and called the adapter
  directly. Fixing it uncovered a second, live defect: the grace round dispatched on the AGENT's
  context while `LlmHandle::stream` installs its serving hop on the LOOP's, so whenever another
  round was open the grace round failed with "a listener short-circuited the chain" and the jot was
  ALWAYS synthetic. Both are covered by
  `reconstruct.rs::the_grace_step_is_ledgered_and_runs_the_agent_request_waterfall`.
- **`status` is the driver-wide interval.** The first wake publishes `Running`, the last one to
  finish publishes `Idle`. That broke the pending-wake flag, which used to be cleared as a side
  effect of the status edge; every driver now says `AgentCell::wake_started()` per wake.
  `preemption.rs::when_idle_does_not_return_while_a_second_wake_is_still_open`.
- **Cancellation reaches a running tool.** `ToolsHandle::execute_under` takes the caller's token;
  the loop passes one that fires on the wake's interrupt or the agent's cancel.
  `preemption.rs::an_interrupt_reaches_a_tool_that_is_already_running` (verified to FAIL against
  the old `execute`).
- **The kernel publishes the composition BEFORE reconciling the tree.** `agent-loop` now fails
  loud when no fingerprint is resolvable, and that exposed the fact that the fingerprint was
  published after `update_tree`, so no plugin's `apply` had ever seen one on the first load.

### 7.2 Deviations that stand

- **§5's drawn order puts `request/header` (7) before the `agent/request` waterfall (8); the code
  appends the header AFTER.** The header records the call config, and the call config is what the
  waterfall decides, so the drawn order cannot be implemented as drawn without writing a header
  that is wrong. Consequence, stated: an `agent/request` listener runs before the durable header
  for its step exists. `wake.rs`'s module comment keeps §5's numbering and marks the swap.
- **P2-D15's JOIN does not restart the step.** A joining message is answered at the next step
  boundary; nothing cancels the in-flight request and `StepOutcome::Restarted` is unused. §5's
  letter holds, its latency intent does not. Unchanged from the build; recorded here and in
  `BUILD.md`.
- **`ToolCall` carries no `StepId`.** `tool-actions` synthesises `"{wake}#{step_index}"` and
  `tool-spawn_worker` `"toolcall:{call.id}"`. The consequence is real and named: the actions idem
  key is not keyed on a ledger step, two calls to the same target inside one step collide as a
  Duplicate, and Phase 8's reconciliation cannot join a journal row back to its `tool/call` row.
  Carrying a `StepId` on `ToolCall` is a seam change and belongs with Phase 5's graph ops.
- **`preemption.rs::a_preempted_wake_skips_its_about_line_refresh` does not mount `about-line`.**
  It proves `refresh`'s reason guard. The mounted row over both drivers is covered by
  `loop_swap.rs`; the map says so rather than the parenthetical claiming otherwise.
- **`wake/max-tokens` and `aborted{cause}`** exist in code with no named test.
- **`WorkerResult.steps` / `.usage`** are always zero: no seam reports them back to a provider.
- **`actions` has no Providers** (Phase 6), so every kind is refused; its invariant coverage is the
  pure `evaluate` plus planted-violation unit tests, never a real boot.
- **Phase 0's deferrals are untouched.** The fiber lifecycle is still a poll loop (P2-D24 measured
  the wake path at p50 ~17ms and found the poll is not the dominant term), and `emit` is still
  spawned and unawaited — which is why nothing durable rides an emit (P2-D25).
