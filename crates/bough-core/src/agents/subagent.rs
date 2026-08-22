//! The subagent launch path (port of `src/agents/subagent.ts`) — the one
//! place a delegated session comes into being.
//!
//! THE INVARIANT THIS HOLDS: **a subagent starts from nothing but its task.**
//! It is a real session (`kind: "subagent"`) with `parentId: null`, and that
//! null is the whole feature. `db.thread_for` is "every ancestor's messages,
//! then my own", so a parent pointer would silently hand the child the
//! spawner's entire conversation. With `parentId: null` the child's thread is
//! exactly the one message this module seeds.
//!
//! Three things DO cross the boundary, deliberately: (1) the lineage edge
//! (`originId`/`originMessageId`); (2) the checkout — the child works in the
//! SAME workspace, nothing to merge; (3) the MCP grant, captured at spawn
//! time. NOTE: (3) is deferred until the mcp wave — the Rust `AppCtx` does not
//! yet carry a grant field, and nothing consumes one before mcp ports
//! (PORT_PLAN rows 3.1–3.3).
//!
//! The depth cap IS here, because it is derived from the lineage this module
//! writes: checked against the LINEAGE (`subagent_depth`), never
//! `TurnCtx.depth`, which is a tier flag that is 1 for any subagent however
//! nested.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::{BoxFuture, Shared};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::errors::{BoughError, ErrorKind};
use crate::schema::events::{EventInput, EventType};
use crate::schema::parts::{Message, Part, Role, Session, SessionKind, Turn, TurnStatus};
use crate::turn::runner::{begin_turn, interrupt_turn, StartedTurn, TurnDeps};
use crate::types::{AppCtx, Clock, Db, Effort, SharedDb, TurnCtx};

use super::with_db;

// ---------------------------------------------------------------------------
// Caps that belong to lineage
// ---------------------------------------------------------------------------

/// Nesting cap. A root (lineage depth 0) may spawn subagents (1), which may
/// delegate one level further (2); depth 2 is terminal (spec §7).
pub const MAX_SUBAGENT_DEPTH: u32 = 2;

/// How many `subagent` hops separate this session from the top of its tree.
/// 0 for a root, fork or compaction. Pure over the database. The hop cap (16)
/// stops a bad lineage write from hanging every later launch.
pub fn subagent_depth(db: &dyn Db, session_id: &str) -> u32 {
    let mut depth = 0;
    let mut cur = db.get_session(session_id).ok().flatten();
    while let Some(s) = &cur {
        if s.kind != SessionKind::Subagent || depth >= 16 {
            break;
        }
        depth += 1;
        cur = match &s.origin_id {
            Some(origin) => db.get_session(origin).ok().flatten(),
            None => None,
        };
    }
    depth
}

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// What a session with neither a given name nor a usable task line is called.
pub const UNTITLED: &str = "untitled";

/// The task-derived title's budget, in characters.
pub const TASK_STUB_CHARS: usize = 40;

/// A spawner-supplied name, cleaned for use as a branch title.
///
/// Control characters and newlines are stripped because this string is
/// rendered straight into the rail, the finished card and the session picker.
/// Returns `None` for a name that is absent or empty once cleaned, so the
/// caller falls back to the task stub. A non-string non-null value is the
/// model's own bad call — an `AgentError` 400.
pub fn clean_subagent_name(name: Option<&Value>) -> Result<Option<String>, BoughError> {
    let raw = match name {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s,
        Some(_) => {
            return Err(BoughError::http(
                400,
                ErrorKind::Agent,
                "agent/spawn(task, {name}): name must be a string",
            ))
        }
    };
    let stripped: String = raw
        .chars()
        .map(|c| {
            if ('\u{0}'..='\u{1f}').contains(&c) || c == '\u{7f}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let flat = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return Ok(None);
    }
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= 48 {
        Ok(Some(flat))
    } else {
        let head: String = chars[..47].iter().collect();
        Ok(Some(format!("{}…", head.trim_end())))
    }
}

/// The default name: the task's first line, word-truncated to ~40 characters.
/// The cut lands on a word boundary unless that would throw away most of the
/// budget, in which case a hard cut reads better than two words.
pub fn task_stub_title(task: &str) -> String {
    let line = task
        .trim()
        .split('\n')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if line.is_empty() {
        return UNTITLED.to_string();
    }
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= TASK_STUB_CHARS {
        return line;
    }
    let cut: String = chars[..TASK_STUB_CHARS].iter().collect();
    let at = cut.rfind(' ');
    let kept = match at {
        Some(at) if at > TASK_STUB_CHARS / 2 => &cut[..at],
        _ => cut.as_str(),
    };
    format!("{}…", kept.trim_end())
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// What the spawner asked for — the `{name}` bag of `agent(task, {name})`.
/// `name` stays an untyped `Value` so a non-string reaches
/// [`clean_subagent_name`]'s teaching error rather than a serde failure.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SubagentOptions {
    #[serde(default)]
    pub name: Option<Value>,
    /// Pin the child to a different model. Absent = the spawning turn's own.
    #[serde(default)]
    pub model: Option<String>,
    /// Thinking depth for the child. Absent = the spawning turn's own.
    #[serde(default)]
    pub effort: Option<Effort>,
}

/// How a subagent's turn ended, as the spawner learns it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Done,
    Error,
    Interrupted,
    Orphaned,
}

/// `status` is carried alongside `ok` because "failed" is not one fact: an
/// errored child, one the user stopped, and one the server restarted under
/// call for different responses from the parent.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentResult {
    pub session_id: String,
    pub title: String,
    /// The turn ran to completion: no error, no interrupt, no orphaning.
    pub ok: bool,
    pub status: SubagentStatus,
    /// The child's final text — its whole report. Never empty.
    pub report: String,
    /// Paths the child changed. Empty until the write-log seam is wired in.
    pub changed_files: Vec<String>,
}

/// The result, shareable: awaited by a blocking `agent()`, watched by the
/// note pipeline, and settled-on by the cap lease — one computation, many
/// consumers, like the TS promise.
pub type SubagentResultFuture = Shared<BoxFuture<'static, SubagentResult>>;

/// What a launch returns: the handle *now*, and the result *later*. A
/// detached `spawn()` answers the program with the handle and never awaits
/// the future; a blocking `agent()` awaits it; the workflow engine needs the
/// session id before completion.
pub struct SubagentLaunch {
    pub session_id: String,
    pub title: String,
    pub session: Session,
    /// The seeded task message — the child's ENTIRE thread at this instant.
    pub task_message: Message,
    /// The child's pending supervisor message; its text becomes the report.
    pub message_id: String,
    pub result: SubagentResultFuture,
}

impl std::fmt::Debug for SubagentLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentLaunch")
            .field("session_id", &self.session_id)
            .field("title", &self.title)
            .field("message_id", &self.message_id)
            .finish()
    }
}

impl super::caps::LeasedLaunch for SubagentLaunch {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }
    fn settled(&self) -> BoxFuture<'static, ()> {
        let f = self.result.clone();
        async move {
            let _ = f.await;
        }
        .boxed()
    }
}

/// How the child's turn is started. [`begin_turn`] satisfies this.
pub type BeginTurn =
    Arc<dyn Fn(&AppCtx, &str, TurnDeps) -> Result<StartedTurn, BoughError> + Send + Sync>;

/// The paths the child changed, for its report. A seam because a git diff at
/// end would report the union of every concurrent sibling's work; the write
/// verbs know what THEY wrote. Errors are swallowed — the report must survive
/// a git hiccup.
pub type ChangedFiles =
    Arc<dyn Fn(&Session) -> BoxFuture<'static, Result<Vec<String>, BoughError>> + Send + Sync>;

/// The injection seams, so a launch is drivable offline with no worker and no
/// key.
#[derive(Clone, Default)]
pub struct LaunchDeps {
    /// Injected clock. Absent = `ctx.app.now`.
    pub now: Option<Clock>,
    /// The CHILD turn's deps: its program runner, granted host fns, registry.
    pub turn: Option<TurnDeps>,
    /// Defaults to [`begin_turn`].
    pub begin: Option<BeginTurn>,
    /// Wall-clock cap on the child's turn; an overrun is interrupted and
    /// reports `interrupted` rather than hanging the spawner forever. Absent =
    /// `BOUGH_SUBAGENT_TIMEOUT_MS`, then 15 minutes.
    pub timeout_ms: Option<u64>,
    pub changed_files: Option<ChangedFiles>,
}

// ---------------------------------------------------------------------------
// The launch
// ---------------------------------------------------------------------------

/// 15 minutes. Env-overridable so the timeout path is testable without waiting.
fn default_timeout_ms() -> u64 {
    std::env::var("BOUGH_SUBAGENT_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n as u64)
        .unwrap_or(15 * 60_000)
}

/// Create the subagent session, seed its task, and start its turn.
///
/// Ordering here is load-bearing. The session row lands and is announced
/// before the task message, so a client that reconciles by id never sees a
/// message for a session it has not heard of. The task message lands before
/// the turn begins, so the child's first round already has its briefing. And
/// the handle is returned before the turn finishes, which is the difference
/// between a detached spawn and a blocking one.
pub fn launch_subagent(
    ctx: &TurnCtx,
    task: &str,
    opts: &SubagentOptions,
    deps: &LaunchDeps,
) -> Result<SubagentLaunch, BoughError> {
    let db = ctx.app.db.clone();
    let bus = ctx.app.bus.clone();
    let now: Clock = deps.now.clone().unwrap_or_else(|| ctx.app.now.clone());

    if task.trim().is_empty() {
        return Err(BoughError::http(
            400,
            ErrorKind::Agent,
            "agent/spawn(task): task must be a non-empty string — it is the subagent's \
             entire briefing, so it has to name the paths, the constraints and what done means",
        ));
    }

    let spawner = with_db(&db, |d| d.get_session(&ctx.session_id))?.ok_or_else(|| {
        BoughError::http(
            404,
            ErrorKind::Agent,
            format!("spawning session {} not found", ctx.session_id),
        )
    })?;

    let depth = with_db(&db, |d| subagent_depth(d, &ctx.session_id));
    if depth >= MAX_SUBAGENT_DEPTH {
        return Err(BoughError::http(
            400,
            ErrorKind::Agent,
            format!(
                "delegation depth limit ({MAX_SUBAGENT_DEPTH}) reached: this session is already \
                 {depth} level(s) of subagent deep — do the remaining work here rather than \
                 delegating further"
            ),
        ));
    }

    // The spawner's own checkout, verbatim — the stored fact, not a re-lookup.
    let runtime = with_db(&db, |d| d.get_session_runtime(&ctx.session_id))?;
    let workspace = runtime.workspace.unwrap_or_else(|| ctx.workspace.clone());

    let title = clean_subagent_name(opts.name.as_ref())?.unwrap_or_else(|| task_stub_title(task));

    let session = with_db(&db, |d| {
        d.create_session(Session {
            id: Uuid::new_v4().to_string(),
            title: title.clone(),
            kind: SessionKind::Subagent,
            created_at: now(),
            // The invariant, in one field: no inherited thread.
            parent_id: None,
            // The lineage edge — the only record that this branch exists.
            origin_id: Some(ctx.session_id.clone()),
            origin_message_id: Some(ctx.message_id.clone()),
            workspace: Some(workspace.clone()),
            // Which PROJECT this is for — survives a moved workspace.
            origin_dir: spawner.origin_dir.clone().or(Some(workspace.clone())),
            // Deliberately unset: inheriting the spawner's `base` would report
            // the spawner's own work as the child's.
            base: None,
            // Not pinned: the inherited model reaches the child via ctx only,
            // so a later manual continuation follows the global default.
            model: None,
            effort: None,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
        })
    })?;
    bus.publish(EventInput {
        r#type: EventType::SessionCreated,
        session_id: Some(session.id.clone()),
        data: serde_json::to_value(&session).unwrap_or_default(),
    });

    let task_message = with_db(&db, |d| {
        d.create_message(Message {
            id: Uuid::new_v4().to_string(),
            session_id: session.id.clone(),
            role: Role::User,
            parts: vec![Part::Text {
                text: task.to_string(),
            }],
            // Complete the moment it lands; `pending` is the streaming flag.
            pending: false,
            created_at: now(),
        })
    })?;
    index_quietly(&db, &task_message);
    bus.publish(EventInput {
        r#type: EventType::MessageStarted,
        session_id: Some(session.id.clone()),
        data: serde_json::to_value(&task_message).unwrap_or_default(),
    });

    // The child's application context. Narrow on purpose: the injected db,
    // bus, clock and provider client, the spawning turn's resolved model and
    // effort as defaults — and nothing that would tie the child to the
    // spawner's turn (the runner rebuilds sessionId/workspace/cancel/depth).
    let child_app = AppCtx {
        db: db.clone(),
        bus: bus.clone(),
        llm: ctx.app.llm.clone(),
        model: Some(opts.model.clone().unwrap_or_else(|| ctx.model.clone())),
        effort: opts.effort.or(ctx.app.effort),
        now: now.clone(),
        cheap: ctx.app.cheap.clone(),
        host: ctx.app.host.clone(),
        starter: ctx.app.starter.clone(),
        turn_registry: ctx.app.turn_registry.clone(),
        model_defaults_path: ctx.app.model_defaults_path.clone(),
    };

    let mut turn_deps = deps.turn.clone().unwrap_or_default();
    // THE MCP GRANT, CAPTURED HERE. The human's grant to a spawner extends to
    // the subagents doing parts of that same granted work, so the child's turn
    // is handed the spawner's grant RESOLVED AT THIS INSTANT (Live → Inherited):
    // a child that resolved its own would resolve to nothing (it has no
    // activations of its own) and every delegated MCP task would die at the
    // first tool call, while one that re-read the file would pick up grants made
    // after it was spawned. A later manual continuation of this branch starts
    // from the server's own `AppCtx`, carries no grant, and so inherits nothing.
    if let Some(grant) = &ctx.mcp_grant {
        turn_deps.mcp_grant = Some(grant.snapshot(&crate::mcp::manager::mcp_manager().config()));
    }
    let registry = turn_deps
        .registry
        .clone()
        .unwrap_or_else(|| child_app.turn_registry.clone());
    let begin: BeginTurn = deps.begin.clone().unwrap_or_else(|| Arc::new(begin_turn));
    // The task lands BEFORE the turn begins — `begin_turn` reads the thread
    // synchronously.
    let started = begin(&child_app, &session.id, turn_deps)?;
    let message_id = started.message.id.clone();

    // An overrun is interrupted rather than left to run: the spawner is
    // holding a future, and a child that never ends is a turn that never ends
    // above it too. `timed_out` is recorded because the SPAWNER cannot
    // otherwise tell an overrun from a human pressing `x x` on the rail.
    let cap_ms = deps.timeout_ms.unwrap_or_else(default_timeout_ms);
    let timed_out = Arc::new(AtomicBool::new(false));

    let sid = session.id.clone();
    let mid = message_id.clone();
    let changed = deps.changed_files.clone();
    let pipeline_db = db.clone();
    let pipeline_bus = bus.clone();
    let flag = timed_out.clone();
    let done = started.done;
    let result: SubagentResultFuture = async move {
        let mut fired = false;
        {
            let done = done;
            tokio::pin!(done);
            let sleep = tokio::time::sleep(std::time::Duration::from_millis(cap_ms));
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = done.as_mut() => break,
                    _ = sleep.as_mut(), if !fired => {
                        fired = true;
                        flag.store(true, Ordering::SeqCst);
                        interrupt_turn(&sid, &registry);
                    }
                }
            }
        }
        let r = build_result(
            &pipeline_db,
            &sid,
            &mid,
            changed.as_ref(),
            InterruptCause {
                timed_out: fired,
                cap_ms,
            },
        )
        .await;
        // The runner already stamped `outcome_ok`. Announcing it is this
        // module's job: without a `session.updated` the rail keeps rendering a
        // finished branch as live, and the tree never learns the branch failed.
        if let Ok(Some(updated)) = with_db(&pipeline_db, |d| d.get_session(&sid)) {
            pipeline_bus.publish(EventInput {
                r#type: EventType::SessionUpdated,
                session_id: Some(sid.clone()),
                data: serde_json::to_value(&updated).unwrap_or_default(),
            });
        }
        r
    }
    .boxed()
    .shared();
    // Eager like the TS promise: the pipeline runs whether or not anybody
    // awaits the handle's copy.
    tokio::spawn(result.clone());

    Ok(SubagentLaunch {
        session_id: session.id.clone(),
        title: session.title.clone(),
        session,
        task_message,
        message_id,
        result,
    })
}

// ---------------------------------------------------------------------------
// The result
// ---------------------------------------------------------------------------

/// Why an interrupt happened, when this module is the one that caused it.
#[derive(Clone, Copy, Debug, Default)]
pub struct InterruptCause {
    pub timed_out: bool,
    pub cap_ms: u64,
}

/// Assemble what the spawner is told, from what the child actually
/// **persisted** (DB, not the in-memory outcome) — a child whose server died
/// mid-turn has no outcome object, and this still yields a truthful
/// `orphaned`. Never fails: the report is a completion callback's payload.
pub async fn build_result(
    db: &SharedDb,
    session_id: &str,
    message_id: &str,
    changed_files: Option<&ChangedFiles>,
    cause: InterruptCause,
) -> SubagentResult {
    let session = with_db(db, |d| d.get_session(session_id)).ok().flatten();
    let turn = with_db(db, |d| d.turn_for_message(message_id))
        .ok()
        .flatten();
    let status = final_status(turn.as_ref());
    let interrupt_reason = if status != SubagentStatus::Interrupted {
        None
    } else if cause.timed_out {
        Some(format!(
            "It ran past its {}s cap and was stopped. Give the next one less to do, or split it.",
            ((cause.cap_ms as f64) / 1000.0).round() as u64
        ))
    } else {
        Some(
            "It was stopped deliberately — by you, or by someone stopping this turn. \
             Do not just retry it; the reason was a decision, not a fault."
                .to_string(),
        )
    };

    let mut changed: Vec<String> = vec![];
    if let (Some(cb), Some(session)) = (changed_files, session.as_ref()) {
        // Best-effort diff; the report must survive a git hiccup.
        if let Ok(files) = cb(session).await {
            changed = files;
        }
    }

    SubagentResult {
        session_id: session_id.to_string(),
        title: session
            .map(|s| s.title)
            .unwrap_or_else(|| UNTITLED.to_string()),
        ok: status == SubagentStatus::Done,
        status,
        report: report_of(db, message_id, status, interrupt_reason.as_deref()),
        changed_files: changed,
    }
}

/// `running` means the row outlived the process that owned it — that is
/// orphaned. So is a missing row.
fn final_status(turn: Option<&Turn>) -> SubagentStatus {
    match turn.map(|t| t.status) {
        Some(TurnStatus::Done) => SubagentStatus::Done,
        Some(TurnStatus::Error) => SubagentStatus::Error,
        Some(TurnStatus::Interrupted) => SubagentStatus::Interrupted,
        _ => SubagentStatus::Orphaned,
    }
}

/// The child's final text, with a guaranteed non-empty fallback that says WHY.
///
/// An interrupt REASON is **appended, not used as a fallback**: a stopped
/// child often has written something first (this one's whole report was
/// `⏹ Stopped.`) — handing that up with no cause attached left the spawner to
/// invent WHY, and it guessed wrong and retried a deliberate stop.
fn report_of(
    db: &SharedDb,
    message_id: &str,
    status: SubagentStatus,
    interrupt_reason: Option<&str>,
) -> String {
    let text = with_db(db, |d| d.get_message(message_id))
        .ok()
        .flatten()
        .map(|m| {
            m.parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
        .trim()
        .to_string();
    if !text.is_empty() {
        return match interrupt_reason {
            Some(reason) => format!("{text}\n\n{reason}"),
            None => text,
        };
    }
    match status {
        SubagentStatus::Done => "The subagent finished without writing a report.".to_string(),
        SubagentStatus::Error => "The subagent errored before reporting.".to_string(),
        SubagentStatus::Interrupted => match interrupt_reason {
            Some(reason) => format!("The subagent was stopped before reporting. {reason}"),
            None => "The subagent was interrupted before reporting.".to_string(),
        },
        SubagentStatus::Orphaned => {
            "The subagent was orphaned (the server restarted) before reporting.".to_string()
        }
    }
}

/// Keyword search is maintained on insert. A failure to index is a degraded
/// search, never a failed launch.
fn index_quietly(db: &SharedDb, message: &Message) {
    if let Err(err) = with_db(db, |d| d.index_message(message)) {
        tracing::error!(
            "failed to index subagent task message {}: {err}",
            message.id
        );
    }
}

// ---------------------------------------------------------------------------
// Tests — port of `src/agents/subagent.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testkit::{
        gated_llm, recording_llm, seed_spawner, spawner_turn_ctx, AgentsFixture, SPAWNER_SECRET,
    };
    use crate::schema::parts::is_collapsed_kind;
    use crate::turn::testkit::stub_deps;
    use bough_llm::LlmError;
    use serde_json::json;
    use std::sync::Mutex;

    fn launch_deps(f: &AgentsFixture) -> LaunchDeps {
        LaunchDeps {
            turn: Some(TurnDeps {
                registry: Some(f.registry.clone()),
                ..stub_deps()
            }),
            ..Default::default()
        }
    }

    // ---- the invariant ------------------------------------------------------

    #[tokio::test]
    async fn a_launched_subagents_thread_is_its_task_and_nothing_else() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let llm = recording_llm("done: touched one file");
        let ctx = spawner_turn_ctx(&f, &seeded, llm.clone());
        let task = "Rename `foo` to `bar` in src/thing.ts and run the tests.";

        let launch =
            launch_subagent(&ctx, task, &SubagentOptions::default(), &launch_deps(&f)).unwrap();

        // Snapshotted before the turn writes anything: at launch the child's
        // whole thread is the task.
        let at_launch = with_db(&f.db, |d| d.thread_for(&launch.session_id)).unwrap();

        launch.result.clone().await;

        // The task, and the empty supervisor placeholder `begin_turn` opened
        // to answer it. Nothing else — there is no ancestor.
        assert_eq!(
            at_launch.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![Role::User, Role::Supervisor]
        );
        assert!(
            at_launch.iter().all(|m| m.session_id == launch.session_id),
            "every message in the thread is the child's own — nothing is inherited"
        );
        assert_eq!(at_launch[0].id, launch.task_message.id);
        assert_eq!(
            at_launch[0].parts,
            vec![Part::Text {
                text: task.to_string()
            }]
        );
        assert_eq!(
            at_launch[1].parts,
            vec![],
            "the placeholder is empty at launch"
        );
        assert_eq!(
            launch.session.parent_id, None,
            "parentId null is what makes it task-only"
        );

        // And what the MODEL saw: one user message, the task, nothing of the
        // spawner's.
        let calls = llm.calls();
        assert_eq!(calls.len(), 1, "one round: text plus stop");
        let sent = &calls[0].messages;
        assert_eq!(
            sent.len(),
            1,
            "the child's first round carries only its briefing"
        );
        assert_eq!(serde_json::to_value(sent[0].role).unwrap(), json!("user"));
        assert_eq!(
            serde_json::to_value(&sent[0].content).unwrap(),
            json!([{ "type": "text", "text": task }])
        );

        // The strong form: no fragment of the spawner's conversation reached
        // the wire — messages, system, or volatile suffix.
        let payload = format!(
            "{}{}{}",
            serde_json::to_string(&calls[0].messages).unwrap(),
            calls[0].system.clone().unwrap_or_default(),
            calls[0].system_volatile.clone().unwrap_or_default(),
        );
        assert!(
            !payload.contains(SPAWNER_SECRET),
            "the spawner's transcript must not leak into the child's payload"
        );
    }

    #[tokio::test]
    async fn lineage_points_back_at_the_spawning_turn() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let llm = recording_llm("done");
        let ctx = spawner_turn_ctx(&f, &seeded, llm);

        let launch = launch_subagent(
            &ctx,
            "Check the error paths in server/app.ts.",
            &SubagentOptions::default(),
            &launch_deps(&f),
        )
        .unwrap();
        launch.result.clone().await;

        let child = with_db(&f.db, |d| d.get_session(&launch.session_id))
            .unwrap()
            .unwrap();
        assert_eq!(child.kind, SessionKind::Subagent);
        assert_eq!(child.origin_id.as_deref(), Some(seeded.session.id.as_str()));
        assert_eq!(
            child.origin_message_id.as_deref(),
            Some(seeded.supervisor.id.as_str()),
            "originMessageId is the supervisor message that was in flight"
        );

        // The edge is what makes it reachable: collapsed out of the top level,
        // present under its origin.
        assert!(
            is_collapsed_kind(child.kind),
            "subagents collapse under their origin"
        );
        assert_eq!(
            with_db(&f.db, |d| d.sessions_by_origin(&seeded.session.id))
                .unwrap()
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
            vec![child.id.clone()],
            "the drill-in finds it"
        );
        assert!(
            !with_db(&f.db, |d| d.list_sessions())
                .unwrap()
                .iter()
                .filter(|s| !is_collapsed_kind(s.kind))
                .any(|s| s.id == child.id),
            "the top-level listing does not"
        );
    }

    // ---- naming -------------------------------------------------------------

    #[test]
    fn the_name_defaults_to_the_tasks_first_40_characters() {
        assert_eq!(task_stub_title("Audit the handlers"), "Audit the handlers");
        assert_eq!(task_stub_title("  Audit  the\n rest of it "), "Audit the");

        let long = "Review every request handler in the server for missing error paths";
        let stub = task_stub_title(long);
        assert!(
            stub.chars().count() <= 41,
            "{stub:?} fits the budget plus the ellipsis"
        );
        assert!(stub.ends_with('…'));
        assert!(
            long.starts_with(stub.trim_end_matches('…').trim_end()),
            "a prefix of the task"
        );
        // Word boundary, not a mid-word chop.
        assert_eq!(stub, "Review every request handler in the…");

        // A 60-char single word has no boundary worth cutting at.
        assert_eq!(
            task_stub_title(&"x".repeat(60)),
            format!("{}…", "x".repeat(40))
        );
        assert_eq!(task_stub_title("   "), UNTITLED);
    }

    #[test]
    fn a_spawner_supplied_name_wins_and_is_safe_to_render() {
        let clean = |s: &str| clean_subagent_name(Some(&json!(s))).unwrap();
        assert_eq!(
            clean("audit the seatbelt profile").as_deref(),
            Some("audit the seatbelt profile")
        );
        assert_eq!(clean("two\nlines\there").as_deref(), Some("two lines here"));
        assert_eq!(clean("   "), None, "empty once cleaned falls back");
        assert_eq!(clean_subagent_name(None).unwrap(), None);
        assert_eq!(clean(&"y".repeat(80)).unwrap().chars().count(), 48);
        let err = clean_subagent_name(Some(&json!(42))).unwrap_err();
        assert_eq!(err.name(), "AgentError");
        assert_eq!(err.status(), 400);
    }

    #[tokio::test]
    async fn the_given_name_titles_the_branch_otherwise_the_task_stub_does() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let ctx = spawner_turn_ctx(&f, &seeded, recording_llm("done"));

        let named = launch_subagent(
            &ctx,
            "Some very long briefing that would otherwise become the title",
            &SubagentOptions {
                name: Some(json!("seatbelt audit")),
                ..Default::default()
            },
            &launch_deps(&f),
        )
        .unwrap();
        assert_eq!(named.title, "seatbelt audit");
        assert_eq!(
            with_db(&f.db, |d| d.get_session(&named.session_id))
                .unwrap()
                .unwrap()
                .title,
            "seatbelt audit"
        );
        named.result.clone().await;

        let unnamed = launch_subagent(
            &ctx,
            "Fix the flaky test in db.test.ts",
            &SubagentOptions::default(),
            &launch_deps(&f),
        )
        .unwrap();
        assert_eq!(unnamed.title, "Fix the flaky test in db.test.ts");
        unnamed.result.clone().await;
    }

    // ---- what else crosses the boundary -------------------------------------

    #[tokio::test]
    async fn the_child_runs_in_the_spawners_checkout_with_the_spawning_turns_model() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        // The spawner's stored workspace is what the child must inherit.
        with_db(&f.db, |d| {
            d.set_session_workspace(&seeded.session.id, "/tmp/shared-checkout")
        })
        .unwrap();
        let ctx = spawner_turn_ctx(&f, &seeded, recording_llm("done"));

        let captured: Arc<Mutex<Option<TurnCtx>>> = Arc::new(Mutex::new(None));
        let sink = captured.clone();
        let mut turn = TurnDeps {
            registry: Some(f.registry.clone()),
            ..stub_deps()
        };
        turn.program = None;
        turn.program_for = Some(Arc::new(move |c: &TurnCtx| {
            *sink.lock().unwrap() = Some(c.clone());
            crate::turn::testkit::ok_program()
        }));
        let deps = LaunchDeps {
            turn: Some(turn),
            ..Default::default()
        };

        let launch =
            launch_subagent(&ctx, "Do the thing.", &SubagentOptions::default(), &deps).unwrap();
        launch.result.clone().await;

        let child = with_db(&f.db, |d| d.get_session(&launch.session_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            child.workspace.as_deref(),
            Some("/tmp/shared-checkout"),
            "same checkout as the spawner — no worktree, nothing to merge"
        );
        let child_ctx = captured.lock().unwrap().clone().unwrap();
        assert_eq!(child_ctx.workspace, "/tmp/shared-checkout");
        assert_eq!(
            child_ctx.model, "claude-test-model",
            "the spawning turn's model flows in"
        );
        assert_eq!(child_ctx.depth, 1, "the child is a delegated tier");
        assert_eq!(
            child.model, None,
            "inherited, not pinned — a later manual continuation follows the global default"
        );
    }

    #[tokio::test]
    async fn the_launch_announces_the_branch_before_its_first_message() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let ctx = spawner_turn_ctx(&f, &seeded, recording_llm("done"));

        let launch = launch_subagent(
            &ctx,
            "Do the thing.",
            &SubagentOptions::default(),
            &launch_deps(&f),
        )
        .unwrap();
        {
            let events = f.events.lock().unwrap();
            let for_child: Vec<_> = events
                .iter()
                .filter(|e| e.session_id.as_deref() == Some(&launch.session_id))
                .collect();
            assert_eq!(for_child[0].r#type, EventType::SessionCreated);
            assert_eq!(for_child[1].r#type, EventType::MessageStarted);
            assert_eq!(
                for_child[1].data.get("id").and_then(|v| v.as_str()),
                Some(launch.task_message.id.as_str())
            );
        }

        launch.result.clone().await;
        assert!(
            f.events.lock().unwrap().iter().any(|e| {
                e.session_id.as_deref() == Some(&launch.session_id)
                    && e.r#type == EventType::SessionUpdated
            }),
            "and announces the branch again when it finishes, so the rail can retire it"
        );
    }

    // ---- the result ---------------------------------------------------------

    #[tokio::test]
    async fn the_result_carries_the_childs_report_and_its_outcome() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let ctx = spawner_turn_ctx(
            &f,
            &seeded,
            recording_llm("Renamed foo to bar in src/thing.ts; tests pass."),
        );

        let mut deps = launch_deps(&f);
        deps.changed_files = Some(Arc::new(|_s: &Session| {
            async { Ok(vec!["src/thing.ts".to_string()]) }.boxed()
        }));
        let launch = launch_subagent(
            &ctx,
            "Rename foo to bar.",
            &SubagentOptions::default(),
            &deps,
        )
        .unwrap();
        let result = launch.result.clone().await;

        assert_eq!(result.session_id, launch.session_id);
        assert_eq!(result.status, SubagentStatus::Done);
        assert!(result.ok);
        assert_eq!(
            result.report,
            "Renamed foo to bar in src/thing.ts; tests pass."
        );
        assert_eq!(result.changed_files, vec!["src/thing.ts"]);
        assert_eq!(
            with_db(&f.db, |d| d.get_session(&launch.session_id))
                .unwrap()
                .unwrap()
                .outcome_ok,
            Some(true)
        );
    }

    #[tokio::test]
    async fn a_child_whose_turn_errored_reports_not_ok_and_says_why() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let on_fire =
            crate::turn::testkit::scripted_llm(vec![crate::turn::testkit::ScriptedRound {
                throws: Some(LlmError::with("provider is on fire", 400, None)),
                ..Default::default()
            }]);
        let ctx = spawner_turn_ctx(&f, &seeded, on_fire);

        let mut deps = launch_deps(&f);
        if let Some(turn) = deps.turn.as_mut() {
            turn.max_round_retries = Some(0);
        }
        let launch = launch_subagent(
            &ctx,
            "Do the impossible.",
            &SubagentOptions::default(),
            &deps,
        )
        .unwrap();
        let result = launch.result.clone().await;

        assert!(!result.ok);
        assert_eq!(
            result.status,
            SubagentStatus::Error,
            "distinguishable from an interrupt or an orphan"
        );
        assert!(
            result.report.contains("on fire"),
            "the report carries the actual error: {}",
            result.report
        );
        assert_eq!(
            with_db(&f.db, |d| d.get_session(&launch.session_id))
                .unwrap()
                .unwrap()
                .outcome_ok,
            Some(false)
        );
    }

    #[tokio::test]
    async fn a_child_stopped_by_its_wall_clock_reports_interrupted_with_the_cap_named() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let (llm, _release, _started) = gated_llm("never gets here");
        let ctx = spawner_turn_ctx(&f, &seeded, llm);

        let mut deps = launch_deps(&f);
        deps.timeout_ms = Some(20);
        let launch =
            launch_subagent(&ctx, "Sleep forever.", &SubagentOptions::default(), &deps).unwrap();
        let result = launch.result.clone().await;

        assert_eq!(result.status, SubagentStatus::Interrupted);
        assert!(!result.ok);
        // The timeout REASON is appended to whatever the child wrote.
        assert!(
            result.report.contains("cap and was stopped"),
            "{}",
            result.report
        );
        assert!(result.report.contains("less to do"), "{}", result.report);
        assert!(
            !result
                .report
                .to_lowercase()
                .contains("stopped deliberately"),
            "{}",
            result.report
        );
    }

    // ---- refusals -----------------------------------------------------------

    #[tokio::test]
    async fn an_empty_task_is_refused_with_a_message_that_says_what_a_task_is_for() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let ctx = spawner_turn_ctx(&f, &seeded, recording_llm("x"));
        let err = launch_subagent(&ctx, "   ", &SubagentOptions::default(), &launch_deps(&f))
            .unwrap_err();
        assert_eq!(err.name(), "AgentError");
        assert!(err.to_string().contains("entire briefing"));
        assert_eq!(
            with_db(&f.db, |d| d.list_sessions()).unwrap().len(),
            1,
            "nothing was created"
        );
    }

    #[tokio::test]
    async fn delegation_stops_at_the_depth_cap() {
        let f = AgentsFixture::new();
        let root = seed_spawner(&f);
        // root → subagent(1) → subagent(2). Depth 2 may not delegate further.
        let mut origin_id = root.session.id.clone();
        let mut deepest = root.session.clone();
        for i in 0..MAX_SUBAGENT_DEPTH {
            deepest = with_db(&f.db, |d| {
                d.create_session(Session {
                    id: Uuid::new_v4().to_string(),
                    title: format!("level {}", i + 1),
                    kind: SessionKind::Subagent,
                    created_at: 2_000 + i as i64,
                    parent_id: None,
                    origin_id: Some(origin_id.clone()),
                    origin_message_id: Some(root.supervisor.id.clone()),
                    workspace: Some("/tmp/checkout".to_string()),
                    origin_dir: None,
                    base: None,
                    model: None,
                    effort: None,
                    draft: None,
                    context_tokens: None,
                    cached_tokens: None,
                    last_llm_at: None,
                    outcome_ok: None,
                })
            })
            .unwrap();
            origin_id = deepest.id.clone();
        }
        {
            let guard = f.db.lock().unwrap();
            assert_eq!(subagent_depth(&*guard, &root.session.id), 0);
            assert_eq!(subagent_depth(&*guard, &deepest.id), MAX_SUBAGENT_DEPTH);
        }

        let seeded = crate::agents::testkit::SeededSpawner {
            session: deepest,
            supervisor: root.supervisor.clone(),
        };
        let ctx = spawner_turn_ctx(&f, &seeded, recording_llm("x"));
        let err = launch_subagent(
            &ctx,
            "one level too far",
            &SubagentOptions::default(),
            &launch_deps(&f),
        )
        .unwrap_err();
        assert_eq!(err.name(), "AgentError");
        assert!(err.to_string().contains("depth limit (2)"), "{err}");
    }

    /// "Interrupted" alone made a model guess the cause and retry a deliberate
    /// stop. The two causes want opposite responses, so the report says WHY.
    #[tokio::test]
    async fn an_interrupted_subagents_report_says_why_when_the_cause_is_known() {
        let f = AgentsFixture::new();
        let session = with_db(&f.db, |d| {
            d.create_session(Session {
                id: Uuid::new_v4().to_string(),
                title: "child".to_string(),
                kind: SessionKind::Subagent,
                created_at: 1_000,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
        })
        .unwrap();
        let msg = with_db(&f.db, |d| {
            d.create_message(Message {
                id: "m-int".to_string(),
                session_id: session.id.clone(),
                role: Role::Supervisor,
                parts: vec![],
                pending: false,
                created_at: 1,
            })
        })
        .unwrap();
        with_db(&f.db, |d| {
            d.create_turn(Turn {
                id: "t-int".to_string(),
                session_id: session.id.clone(),
                message_id: msg.id.clone(),
                status: TurnStatus::Interrupted,
                step: "run_steps".to_string(),
                created_at: 1,
                updated_at: 2,
                error: None,
                usage: None,
            })
        })
        .unwrap();

        // A child that wrote something before it was stopped STILL carries the
        // reason: the partial text used to short-circuit it.
        let partial = with_db(&f.db, |d| {
            d.create_message(Message {
                id: "m-partial".to_string(),
                session_id: session.id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::Text {
                    text: "⏹ Stopped.".to_string(),
                }],
                pending: false,
                created_at: 3,
            })
        })
        .unwrap();
        with_db(&f.db, |d| {
            d.create_turn(Turn {
                id: "t-partial".to_string(),
                session_id: session.id.clone(),
                message_id: partial.id.clone(),
                status: TurnStatus::Interrupted,
                step: "run_steps".to_string(),
                created_at: 3,
                updated_at: 4,
                error: None,
                usage: None,
            })
        })
        .unwrap();
        let r = build_result(
            &f.db,
            &session.id,
            &partial.id,
            None,
            InterruptCause::default(),
        )
        .await;
        assert!(r.report.contains("⏹ Stopped."));
        assert!(r.report.to_lowercase().contains("stopped deliberately"));

        // A deliberate stop: do not just retry it.
        let stopped =
            build_result(&f.db, &session.id, &msg.id, None, InterruptCause::default()).await;
        assert!(stopped
            .report
            .to_lowercase()
            .contains("stopped deliberately"));
        assert!(stopped.report.to_lowercase().contains("not just retry"));

        // An overrun: the remedy IS to give the next one less to do.
        let over = build_result(
            &f.db,
            &session.id,
            &msg.id,
            None,
            InterruptCause {
                timed_out: true,
                cap_ms: 90_000,
            },
        )
        .await;
        assert!(over.report.contains("90s cap"), "{}", over.report);
        assert!(over.report.to_lowercase().contains("less to do"));
        assert!(
            !over.report.to_lowercase().contains("stopped deliberately"),
            "{}",
            over.report
        );
    }

    #[tokio::test]
    async fn changed_files_errors_are_swallowed_and_the_array_is_copied() {
        let f = AgentsFixture::new();
        let seeded = seed_spawner(&f);
        let ctx = spawner_turn_ctx(&f, &seeded, recording_llm("done"));

        let mut deps = launch_deps(&f);
        deps.changed_files = Some(Arc::new(|_s: &Session| {
            async { Err(BoughError::bad_request("git hiccup")) }.boxed()
        }));
        let launch =
            launch_subagent(&ctx, "Do a thing.", &SubagentOptions::default(), &deps).unwrap();
        let result = launch.result.clone().await;
        assert_eq!(result.status, SubagentStatus::Done);
        assert!(
            result.changed_files.is_empty(),
            "a git hiccup never fails the report"
        );
    }
}
