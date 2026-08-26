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
//!
//! No runtime invariant: this crate mounts no row, owns no event stream and no data relation of
//! its own. What it enforces is enforced per call and asserted by its unit tests; the durable
//! consequences it writes are the ledger rows the HOSTS own, and each host's `invariant` module
//! checks those (`wards-rhai`, `hooks-exec`, `mcp-subprocess`).

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_actions::{ActionKind, ActionRequest, ActionTarget, ActionsHandle};
use bough_plugin_agents::{AgentsHandle, Delivery, Message, Sender};
use bough_plugin_ledger::vocabulary::MailClass;
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, LedgerHandle, Ref, StepId, StepType, WakeId,
};
use bough_plugin_schedule::{Cadence, Job, JobFire, JobName, JobOutcome, JobSpec, ScheduleHandle};
use bough_plugin_tools::{Restrict, ToolName};
use bough_plugin_workers::{SealSpec, StartWorker, WorkerKind, WorkersHandle};
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
    /// What fired the runtime code. See [`Trigger`].
    pub trigger: Trigger,
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

/// The triggering step, as the executor needs it: an action journalled through `ctx.actions`
/// wants the agent it is for, the wake it belongs to and the step that caused it (§7's idem key),
/// and a `Mark` wants a wake to append under.
///
/// P6-D9a (DEVIATION from the plan's `RuntimeCx`): the six kinds name an `agent` per action, but
/// `Act` does not, and `ActionRequest`/`Append` both need a wake and a triggering step. Those three
/// are properties of what FIRED the runtime code, not of an action, so they live here.
#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    pub agent: Option<AgentName>,
    pub wake: WakeId,
    pub step: StepId,
}

impl Trigger {
    /// The trigger a host uses when nothing caused it but the host itself (a boot-time hook).
    pub fn synthetic(source: &RuntimeSource) -> Trigger {
        Trigger {
            agent: None,
            wake: WakeId::new(format!("{}:synthetic", source.sender_label())),
            step: StepId::new(format!("{}:synthetic", source.sender_label())),
        }
    }
}

impl RuntimeLimits {
    /// The caps a host uses when its config carries none. Values are config; the SHAPE is code.
    pub fn modest() -> RuntimeLimits {
        RuntimeLimits {
            max_actions: 8,
            max_spawns: 1,
            max_text_bytes: 4000,
        }
    }
}

/// Intern a `&'static str` for `Sender::System`, the way `agents` does for its own wire form: the
/// set of runtime-code names is small and fixed by the files on disk, so leaking one per distinct
/// name is bounded.
fn intern(s: &str) -> &'static str {
    static TAGS: parking_lot::Mutex<Option<Vec<&'static str>>> = parking_lot::Mutex::new(None);
    let mut tags = TAGS.lock();
    let tags = tags.get_or_insert_with(Vec::new);
    if let Some(found) = tags.iter().find(|t| **t == s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    tags.push(leaked);
    leaked
}

/// PURE: the refusal a bad `Act` earns, without touching the world.
///
/// Two distinct refusals (V10): a kind outside the four does not EXIST as a spelling and is refused
/// here; a kind that exists but has no Provider is refused by the executor as `NoProvider`.
pub fn parse_kind(kind: &str) -> Result<ActionKind, String> {
    ActionKind::all()
        .iter()
        .copied()
        .find(|k| k.as_str() == kind)
        .ok_or_else(|| {
            let known = ActionKind::all()
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("no such action kind `{kind}`; the four are: {known}")
        })
}

/// PURE: apply [`RuntimeLimits`] to a returned list, reporting what was dropped.
pub fn clamp(
    actions: &[RuntimeAction],
    limits: &RuntimeLimits,
) -> (Vec<RuntimeAction>, Vec<String>) {
    let mut reports = Vec::new();
    let mut out: Vec<RuntimeAction> = Vec::new();
    if actions.len() > limits.max_actions {
        reports.push(format!(
            "returned {} actions; max_actions is {} — the rest were dropped",
            actions.len(),
            limits.max_actions
        ));
    }
    let mut spawns = 0usize;
    for a in actions.iter().take(limits.max_actions) {
        if matches!(a, RuntimeAction::Spawn { .. }) {
            spawns += 1;
            if spawns > limits.max_spawns {
                reports.push(format!(
                    "spawn dropped: max_spawns is {}",
                    limits.max_spawns
                ));
                continue;
            }
        }
        out.push(clip(a, limits.max_text_bytes, &mut reports));
    }
    (out, reports)
}

/// Clip every free-text field of one action at `max`, on a char boundary.
fn clip(a: &RuntimeAction, max: usize, reports: &mut Vec<String>) -> RuntimeAction {
    let mut a = a.clone();
    let mut clipped = false;
    let mut cut = |s: &mut String| {
        if s.len() > max {
            let mut end = max;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            clipped = true;
        }
    };
    match &mut a {
        RuntimeAction::Spawn { task, .. } => cut(task),
        RuntimeAction::Mark { text, .. } => cut(text),
        RuntimeAction::Post { subject, text, .. } => {
            cut(subject);
            cut(text)
        }
        RuntimeAction::Hint { text, .. } => cut(text),
        RuntimeAction::Schedule { .. } | RuntimeAction::Act { .. } => {}
    }
    if clipped {
        reports.push(format!("text clipped at max_text_bytes ({max})"));
    }
    a
}

/// Execute in order, STOPPING AT NOTHING: a refusal is recorded and the next action still runs.
///
/// This is the ONLY place a runtime script's intent becomes an effect, so citations, bounds and the
/// write boundary are enforced here and nowhere else (§9).
pub async fn execute_all(
    cx: &RuntimeCx,
    actions: &[RuntimeAction],
    limits: &RuntimeLimits,
) -> Vec<ActionOutcome> {
    let (kept, reports) = clamp(actions, limits);
    let mut out: Vec<ActionOutcome> = Vec::new();
    for a in &kept {
        out.push(execute_one(cx, a).await);
    }
    for r in reports {
        out.push(ActionOutcome::Refused { reason: r });
    }
    out
}

/// One action, through its seam. Never panics; every failure becomes [`ActionOutcome::Refused`].
async fn execute_one(cx: &RuntimeCx, a: &RuntimeAction) -> ActionOutcome {
    match a {
        RuntimeAction::Spawn { agent, task, tools } => {
            do_spawn(cx, agent, task, tools.as_deref()).await
        }
        RuntimeAction::Mark {
            agent,
            mark,
            text,
            cites,
        } => do_mark(cx, agent, *mark, text, cites).await,
        RuntimeAction::Post {
            agent,
            subject,
            text,
            cites,
        } => do_post(cx, agent, subject, text, cites).await,
        RuntimeAction::Hint { agent, text } => do_hint(cx, agent, text).await,
        RuntimeAction::Schedule { name, in_ms, then } => do_schedule(cx, name, *in_ms, then).await,
        RuntimeAction::Act {
            kind,
            target,
            payload,
        } => do_act(cx, kind, target, payload).await,
    }
}

fn refused(reason: impl Into<String>) -> ActionOutcome {
    ActionOutcome::Refused {
        reason: reason.into(),
    }
}

fn cites_of(cites: &[String]) -> Vec<Cite> {
    cites
        .iter()
        .map(|c| Cite {
            r#ref: Ref::new(c.clone()),
            url: None,
        })
        .collect()
}

async fn do_spawn(
    cx: &RuntimeCx,
    agent: &str,
    task: &str,
    tools: Option<&[String]>,
) -> ActionOutcome {
    let name = AgentName::new(agent);
    let Some(live) = cx.agents.by_name(&name) else {
        return refused(format!(
            "no live agent named `{agent}` to spawn a worker for"
        ));
    };
    let req = StartWorker {
        kind: WorkerKind::Spawn,
        spawner: name.clone(),
        spawner_id: live.id().clone(),
        wake: cx.trigger.wake.clone(),
        step: cx.trigger.step.clone(),
        // Bounds are the Definition's: depth comes from the registry, never from the script.
        depth: cx.workers.depth_of(&name).saturating_add(1),
        task: task.to_string(),
        seal: SealSpec::report(),
        tools: tools.map(|names| Restrict {
            allow: Some(
                names
                    .iter()
                    .map(|n| ToolName::new(n.clone()))
                    .collect::<BTreeSet<_>>(),
            ),
            deny: BTreeSet::new(),
        }),
        ask_mode: cx.workers.default_ask_mode(),
        at: cx.at,
    };
    match cx.workers.start(&cx.ctx, req).await {
        Ok(r) => ActionOutcome::Did {
            detail: format!("spawned worker {} on `{agent}`", r.worker),
        },
        Err(e) => refused(format!("spawn refused: {e}")),
    }
}

async fn do_mark(
    cx: &RuntimeCx,
    agent: &str,
    kind: MarkKind,
    text: &str,
    cites: &[String],
) -> ActionOutcome {
    // §10: a claim is a claim only if it cites. Checked FIRST — before any lookup — so the refusal
    // names the rule rather than an incidental missing referent, and so it holds with no tree.
    if kind == MarkKind::Claim && cites.is_empty() {
        return refused(format!(
            "a claim must cite its evidence; `{}` proposed one with no cites",
            cx.source.sender_label()
        ));
    }
    let name = AgentName::new(agent);
    let Some(live) = cx.agents.by_name(&name) else {
        return refused(format!("no live agent named `{agent}` to mark"));
    };
    let (step_kind, class, body) = match kind {
        MarkKind::Claim => (
            "claim/proposed",
            Class::Thought,
            serde_json::json!({
                "claim": format!("{}:{}", cx.source.sender_label(), cx.trigger.step),
                "kind": "observation",
                "title": first_line(text),
                "body": text,
            }),
        ),
        MarkKind::Pin => (
            "pin/set",
            Class::Thought,
            serde_json::json!({ "title": first_line(text), "text": text, "supersedes": [] }),
        ),
    };
    let append = Append {
        traj: live.traj().clone(),
        wake: cx.trigger.wake.clone(),
        kind: StepType::new(step_kind),
        class,
        body,
        cites: cites_of(cites),
        at: cx.at,
        id: None,
    };
    match cx.ledger.0.append(append).await {
        Ok(s) => ActionOutcome::Did {
            detail: format!("appended {step_kind} {} on `{agent}`", s.id),
        },
        Err(e) => refused(format!("mark refused: {e}")),
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

async fn do_post(
    cx: &RuntimeCx,
    agent: &str,
    subject: &str,
    text: &str,
    cites: &[String],
) -> ActionOutcome {
    // `mail/delivered` is EVIDENCE: mail that cannot say where it came from is not deliverable
    // (§3). The refusal is spelled here so a ward author reads a rule, not a schema error.
    if cites.is_empty() {
        return refused(format!(
            "a post must cite what it is about; `{}` posted `{subject}` with no cites",
            cx.source.sender_label()
        ));
    }
    let name = AgentName::new(agent);
    let Some(live) = cx.agents.by_name(&name) else {
        return refused(format!("no live agent named `{agent}` to post to"));
    };
    let cites = cites_of(cites);
    let refs: BTreeSet<Ref> = cites.iter().map(|c| c.r#ref.clone()).collect();
    let mail = Delivery {
        from: Sender::System(intern(&cx.source.sender_label())),
        // Runtime code never wakes a lane: §5's urgencies are the collectors' to choose.
        class: MailClass::Ordinary,
        subject: subject.to_string(),
        summary: first_line(text),
        text: text.to_string(),
        cites,
        refs,
        at: cx.at,
    };
    match live.deliver(mail).await {
        Ok(_) => ActionOutcome::Did {
            detail: format!("posted `{subject}` into `{agent}`"),
        },
        Err(e) => refused(format!("post refused: {e}")),
    }
}

async fn do_hint(cx: &RuntimeCx, agent: &str, text: &str) -> ActionOutcome {
    let name = AgentName::new(agent);
    let Some(live) = cx.agents.by_name(&name) else {
        return refused(format!("no live agent named `{agent}` to hint"));
    };
    let msg = Message::new(
        Sender::System(intern(&cx.source.sender_label())),
        &format!("hint from {}", cx.source.sender_label()),
        text,
        cx.at,
    );
    match live.inject(msg).await {
        Ok(_) => ActionOutcome::Did {
            detail: format!("hinted `{agent}`"),
        },
        Err(e) => refused(format!("hint refused: {e}")),
    }
}

/// A one-shot job that replays `then` ONCE and then does nothing, however often it fires.
struct OneShot {
    cx: RuntimeCx,
    then: RuntimeAction,
    done: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl Job for OneShot {
    async fn run(&self, _fire: JobFire) -> JobOutcome {
        if self.done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return JobOutcome::Ran {
                detail: "already replayed".into(),
            };
        }
        match execute_one(&self.cx, &self.then).await {
            ActionOutcome::Did { detail } => JobOutcome::Ran { detail },
            ActionOutcome::Refused { reason } => JobOutcome::Failed { error: reason },
        }
    }
}

async fn do_schedule(
    cx: &RuntimeCx,
    name: &str,
    in_ms: u64,
    then: &RuntimeAction,
) -> ActionOutcome {
    // NO NESTING: a schedule that schedules is a loop with a script's name on it, and the point of
    // this vocabulary is that a script cannot build one.
    if matches!(then, RuntimeAction::Schedule { .. }) {
        return refused("a scheduled action may not itself be a schedule");
    }
    if in_ms == 0 {
        return refused("a schedule needs a non-zero delay");
    }
    let job_name = JobName::new(format!("{}:{name}", cx.source.sender_label()));
    let spec = JobSpec {
        name: job_name.clone(),
        cadence: Cadence::Every { every_ms: in_ms },
        catch_up: false,
        job: Arc::new(OneShot {
            cx: cx.clone(),
            then: then.clone(),
            done: std::sync::atomic::AtomicBool::new(false),
        }),
    };
    match cx.schedule.0.register(&cx.ctx, spec).await {
        Ok(handle) => {
            // The registration is an effect of the CALLER's fiber (`ctx.effect` pushed it there),
            // so dropping the token here leaves the job registered and the row's unload still
            // takes it away.
            drop(handle);
            ActionOutcome::Did {
                detail: format!("scheduled `{job_name}` in {in_ms}ms"),
            }
        }
        Err(e) => refused(format!("schedule refused: {e}")),
    }
}

async fn do_act(
    cx: &RuntimeCx,
    kind: &str,
    target: &str,
    payload: &serde_json::Value,
) -> ActionOutcome {
    let kind = match parse_kind(kind) {
        Ok(k) => k,
        Err(e) => return refused(e),
    };
    let Some(agent) = cx.trigger.agent.clone() else {
        return refused(format!(
            "`{}` has no agent to act as; an outward act is always somebody's",
            cx.source.sender_label()
        ));
    };
    let req = ActionRequest {
        kind,
        target: ActionTarget::new(target.to_string()),
        payload: payload.clone(),
        agent,
        wake: cx.trigger.wake.clone(),
        step: cx.trigger.step.clone(),
        at: cx.at,
    };
    match cx.actions.execute(&cx.ctx, req).await {
        Ok(a) => ActionOutcome::Did {
            detail: format!("{} {} → {}", kind.as_str(), target, a.locator),
        },
        Err(e) => refused(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::{EffectHandle, KernelCore, PluginError};
    use bough_plugin_ledger_memory::store::MemoryStore;
    use bough_plugin_schedule::{JobInfo, JobRun, ScheduleError, Scheduler};
    use bough_plugin_workers::Bounds;
    use chrono::TimeZone;

    /// A Scheduler that registers nothing and fires nothing: WP-1's Providers are not this crate's
    /// subject, and a test that needed one would be testing them.
    struct NoScheduler;

    #[async_trait::async_trait]
    impl Scheduler for NoScheduler {
        fn provider(&self) -> &'static str {
            "test-null"
        }
        async fn register(
            &self,
            _ctx: &Context,
            _spec: JobSpec,
        ) -> Result<EffectHandle, PluginError> {
            unreachable!("no test registers a job")
        }
        fn jobs(&self) -> Vec<JobInfo> {
            Vec::new()
        }
        async fn fire_now(&self, name: &JobName) -> Result<JobRun, ScheduleError> {
            Err(ScheduleError::Unknown(name.clone()))
        }
    }

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    /// A cx over real seams with NO Provider on any of them: the write boundary as a fresh tree
    /// has it. Every refusal a test here asserts is therefore the seam's own.
    fn cx() -> RuntimeCx {
        let ctx = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
        RuntimeCx {
            ctx: ctx.clone(),
            agents: bough_plugin_agents::AgentsHandle::new(ctx, ledger.clone()),
            ledger: ledger.clone(),
            workers: bough_plugin_workers::WorkersHandle::new(Bounds {
                max_in_flight: 8,
                max_depth: 3,
                per_wake_spawn_cap: 4,
            }),
            actions: bough_plugin_actions::ActionsHandle::new(ledger),
            schedule: ScheduleHandle(Arc::new(NoScheduler)),
            source: RuntimeSource::Ward("reviews".into()),
            trigger: Trigger {
                agent: Some(AgentName::new("sol")),
                wake: WakeId::new("w1"),
                step: StepId::new("s1"),
            },
            at: at(),
        }
    }

    fn hint(text: &str) -> RuntimeAction {
        RuntimeAction::Hint {
            agent: "sol".into(),
            text: text.into(),
        }
    }

    #[test]
    fn a_kind_outside_the_four_is_refused_by_name() {
        let e = parse_kind("slack_send").unwrap_err();
        assert!(e.contains("slack_send"), "{e}");
        assert!(e.contains("no such action kind"), "{e}");
    }

    #[test]
    fn each_of_the_four_parses() {
        assert_eq!(parse_kind("open_pr").unwrap(), ActionKind::OpenPr);
        assert_eq!(parse_kind("push_to_pr").unwrap(), ActionKind::PushToPr);
        assert_eq!(
            parse_kind("bot_thread_op").unwrap(),
            ActionKind::BotThreadOp
        );
        assert_eq!(parse_kind("linear_write").unwrap(), ActionKind::LinearWrite);
    }

    #[tokio::test]
    async fn an_act_no_provider_claims_is_refused_and_the_next_action_still_runs() {
        let cx = cx();
        let actions = vec![
            RuntimeAction::Act {
                kind: "open_pr".into(),
                target: "owner/repo".into(),
                payload: serde_json::json!({}),
            },
            RuntimeAction::Mark {
                agent: "sol".into(),
                mark: MarkKind::Claim,
                text: "no cites here".into(),
                cites: vec![],
            },
        ];
        let out = execute_all(&cx, &actions, &RuntimeLimits::modest()).await;
        assert_eq!(out.len(), 2, "{out:?}");
        match &out[0] {
            ActionOutcome::Refused { reason } => {
                assert!(reason.contains("open_pr"), "{reason}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // The SECOND action ran: `execute_all` stops at nothing.
        assert!(matches!(out[1], ActionOutcome::Refused { .. }), "{out:?}");
    }

    #[tokio::test]
    async fn a_spelling_outside_the_four_is_refused_before_the_seam() {
        let cx = cx();
        let out = execute_all(
            &cx,
            &[RuntimeAction::Act {
                kind: "slack_send".into(),
                target: "#eng".into(),
                payload: serde_json::json!({ "text": "hi" }),
            }],
            &RuntimeLimits::modest(),
        )
        .await;
        match &out[0] {
            ActionOutcome::Refused { reason } => {
                assert!(
                    reason.contains("no such action kind `slack_send`"),
                    "{reason}"
                )
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Nothing was journalled: a refused spelling never became an intent.
        assert!(cx.actions.pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_claim_with_no_cites_is_refused_naming_the_rule() {
        let cx = cx();
        let out = execute_all(
            &cx,
            &[RuntimeAction::Mark {
                agent: "sol".into(),
                mark: MarkKind::Claim,
                text: "the build is green".into(),
                cites: vec![],
            }],
            &RuntimeLimits::modest(),
        )
        .await;
        match &out[0] {
            ActionOutcome::Refused { reason } => assert!(reason.contains("must cite"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn max_actions_truncates_and_reports() {
        let cx = cx();
        let limits = RuntimeLimits {
            max_actions: 2,
            max_spawns: 1,
            max_text_bytes: 100,
        };
        let out = execute_all(&cx, &[hint("a"), hint("b"), hint("c")], &limits).await;
        // Two attempts, then ONE report line naming the cap.
        assert_eq!(out.len(), 3, "{out:?}");
        match &out[2] {
            ActionOutcome::Refused { reason } => {
                assert!(reason.contains("max_actions"), "{reason}")
            }
            other => panic!("expected the cap report, got {other:?}"),
        }
    }

    #[test]
    fn max_spawns_drops_the_extra_spawns_and_reports() {
        let spawn = |t: &str| RuntimeAction::Spawn {
            agent: "sol".into(),
            task: t.into(),
            tools: None,
        };
        let (kept, reports) = clamp(
            &[spawn("one"), spawn("two"), hint("x")],
            &RuntimeLimits {
                max_actions: 8,
                max_spawns: 1,
                max_text_bytes: 100,
            },
        );
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(matches!(kept[1], RuntimeAction::Hint { .. }));
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("max_spawns"), "{reports:?}");
    }

    #[test]
    fn text_is_clipped_at_max_text_bytes_on_a_char_boundary() {
        let (kept, reports) = clamp(
            &[hint("ααααα")],
            &RuntimeLimits {
                max_actions: 8,
                max_spawns: 1,
                max_text_bytes: 5,
            },
        );
        match &kept[0] {
            RuntimeAction::Hint { text, .. } => assert_eq!(text, "αα"),
            other => panic!("{other:?}"),
        }
        assert!(reports[0].contains("max_text_bytes"), "{reports:?}");
    }

    #[test]
    fn the_wire_form_round_trips_and_refuses_an_unknown_field() {
        let a: RuntimeAction = serde_json::from_value(serde_json::json!({
            "kind": "act", "action_kind": "open_pr", "target": "o/r"
        }))
        .unwrap();
        assert_eq!(
            a,
            RuntimeAction::Act {
                kind: "open_pr".into(),
                target: "o/r".into(),
                payload: serde_json::Value::Null
            }
        );
        assert!(serde_json::from_value::<RuntimeAction>(serde_json::json!({
            "kind": "hint", "agent": "sol", "text": "x", "extra": 1
        }))
        .is_err());
    }
}
