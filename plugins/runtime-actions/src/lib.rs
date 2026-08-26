//! Invariant: runtime code (a ward, a hook executable, a subprocess plugin) RETURNS actions and
//! PERFORMS NONE. This crate is the only place those returned actions become effects, and it is
//! where citations, bounds and the write boundary are enforced (§9). A script cannot reach a seam
//! except through [`execute_all`].
//!
//! Two distinct refusals, and the map (V10) names which is which:
//!   - `kind` does not deserialize into an `ActionKind` ⇒ [`parse_kind`] refuses it BEFORE the
//!     executor: "no such action kind `slack_send`".
//!   - it does, but no Provider registered it ⇒ `ActionError::NoProvider`, from the executor.
//!
//! NO ROW: a library the three hosts share.

use bough_kernel::Context;
use bough_plugin_actions::{ActionKind, ActionsHandle};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_ledger::LedgerHandle;
use bough_plugin_schedule::ScheduleHandle;
use bough_plugin_workers::WorkersHandle;
use chrono::{DateTime, Utc};

/// What runtime code may ask the harness to do.
///
/// Six kinds. §9 names five (spawn, mark, post, hint, schedule) and then names `ctx.actions` among
/// the seams the host executes through, which only makes sense with a sixth that reaches it
/// (P6-D9).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeAction {
    /// → `ctx.workers.start`. Bounds are the Definition's, not the script's.
    Spawn {
        agent: String,
        task: String,
        #[serde(default)]
        tools: Option<Vec<String>>,
    },
    /// → `ctx.ledger.append` of `claim/proposed` or `pin/set`. Cites REQUIRED for a claim.
    Mark {
        agent: String,
        mark: MarkKind,
        text: String,
        #[serde(default)]
        cites: Vec<String>,
    },
    /// → `Agent::deliver`, `Sender::System("ward:<name>")`, `MailClass::Ordinary`. Into a lane's
    /// OWN chat. There is no outward `post`.
    Post {
        agent: String,
        subject: String,
        text: String,
        #[serde(default)]
        cites: Vec<String>,
    },
    /// → `Agent::inject` (a next-step steer). A nudge, not mail.
    Hint { agent: String, text: String },
    /// → `ctx.schedule.register` of a ONE-SHOT job replaying `then`.
    Schedule {
        name: String,
        in_ms: u64,
        then: Box<RuntimeAction>,
    },
    /// → `ctx.actions.execute`. THE ONLY KIND THAT REACHES THE WORLD.
    ///
    /// `kind` is a STRING on purpose: a script may spell anything, and the refusal is the point.
    ///
    /// The field is spelled `action_kind` ON THE WIRE: `kind` is the enum's own internal tag, and
    /// serde refuses a variant field that shadows it. The Rust name stays `kind`.
    Act {
        #[serde(rename = "action_kind")]
        kind: String,
        target: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

/// What a `Mark` writes.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MarkKind {
    Claim,
    Pin,
}

/// Everything the executor needs, INJECTED. No clock, no globals.
#[derive(Clone)]
pub struct RuntimeCx {
    pub ctx: Context,
    pub agents: AgentsHandle,
    pub ledger: LedgerHandle,
    pub workers: WorkersHandle,
    pub actions: ActionsHandle,
    pub schedule: ScheduleHandle,
    pub source: RuntimeSource,
    pub at: DateTime<Utc>,
}

/// Which piece of runtime code returned the actions. Names the sender and the journal entry.
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeSource {
    Ward(String),
    Hook(String),
    Process(String),
}

impl RuntimeSource {
    /// The `ward:<name>` / `hook:<name>` / `process:<name>` spelling a post is sent under.
    ///
    /// Merge note 6 asks for `Sender::Ward(String)` / `Sender::Hook(String)`; until then this
    /// interns a `&'static str` per distinct name.
    pub fn sender_label(&self) -> String {
        match self {
            RuntimeSource::Ward(n) => format!("ward:{n}"),
            RuntimeSource::Hook(n) => format!("hook:{n}"),
            RuntimeSource::Process(n) => format!("process:{n}"),
        }
    }
}

/// What one action did.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionOutcome {
    Did { detail: String },
    Refused { reason: String },
}

/// Caps every host applies before executing anything a script returned. NOT a script knob.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLimits {
    pub max_actions: usize,
    pub max_spawns: usize,
    pub max_text_bytes: usize,
}

/// Execute in order, STOPPING AT NOTHING: a refusal is recorded and the next action still runs.
/// WP-6.
pub async fn execute_all(
    cx: &RuntimeCx,
    actions: &[RuntimeAction],
    limits: &RuntimeLimits,
) -> Vec<ActionOutcome> {
    let _ = (cx, actions, limits);
    todo!("WP-6")
}

/// PURE: the refusal a bad `Act` earns, without touching the world. WP-6.
pub fn parse_kind(kind: &str) -> Result<ActionKind, String> {
    let _ = kind;
    todo!("WP-6: `no such action kind `slack_send`` for anything outside the four")
}

/// PURE: apply [`RuntimeLimits`] to a returned list, reporting what was dropped. WP-6.
pub fn clamp(
    actions: &[RuntimeAction],
    limits: &RuntimeLimits,
) -> (Vec<RuntimeAction>, Vec<String>) {
    let _ = (actions, limits);
    todo!("WP-6: truncate at `max_actions`, cap spawns at `max_spawns`, clip text at `max_text_bytes`")
}
