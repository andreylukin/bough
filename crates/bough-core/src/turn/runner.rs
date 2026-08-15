//! The turn loop (port of `src/turn/runner.ts`): everything that happens after
//! a user message lands.
//!
//! THE INVARIANT THIS HOLDS: **a turn always ends, always ends visibly, and
//! always ends exactly once.** Three separate failures hide behind that one
//! sentence, and every structural decision in this file is one of them:
//!
//! 1. **A turn never ends implicitly.** The model calls `stop` after its final
//!    text, in the same response. A response that just trails off is not an
//!    ending — it is a model that forgot — so it gets nudged, with a bounded
//!    count so a stop-incapable model cannot loop the API forever. The nudges
//!    and the `stop` call itself are loop control, never content: they live
//!    only in this turn's in-memory exchange and are never persisted, so the
//!    thread and every future replay stay clean.
//! 2. **Every turn must produce user-visible text.** A turn of nothing but
//!    tool calls shows the user a stack of collapsed cards and no answer — the
//!    agent looks mute. Worse, narration counts for nothing: a turn that says
//!    "let me implement the changes:" and then ends on a raw `rg` dump has
//!    said less than one that said nothing. So [`said_something`] asks only
//!    about text *after the last tool call*, and a turn about to end mute is
//!    asked once for a closing report, then forced into a text-only round
//!    (`toolChoice: "none"`) — which reliably yields prose where a second
//!    nudge yields another empty stop.
//! 3. **The pending message is closed on every path.** Success, failure,
//!    interrupt, a crash in the loop — `pending` goes false and
//!    `message.finished` fires, because a message left pending is a session
//!    the UI shows as busy forever and a queue that never drains.
//!
//! WHAT IS NOT HERE, DELIBERATELY. There is no acceptance gate: the harness
//! does not re-run a committed check, does not grade `done`, and does not
//! block completion. `run_steps`'s `done` flag is the model's own statement
//! that the work is finished, and it is recorded with the call and acted on by
//! nobody.
//!
//! PROVIDER-BLINDNESS. Nothing in this file knows which provider it is talking
//! to. Everything goes through `LlmClient` — if a provider name ever appears
//! below, it has leaked, and it will leak everywhere next.
//!
//! REASONING, AND THE ONE PLACE IT IS ECHOED. Across turns, reasoning is
//! dropped (`replay.rs`). *Within* one turn the block goes back verbatim,
//! `meta` and all, because a provider that signs thinking rejects a tool call
//! whose thinking was altered. The two rules are not in tension: the in-turn
//! echo comes from `LlmResult.content` in memory, the cross-turn drop is about
//! what `replay.rs` reads out of the database, and nothing signed is ever
//! stored incorrectly — reasoning is persisted WITH `meta` and the signing
//! `model` so the next turn can replay it to the same model.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use futures::future::BoxFuture;
use futures::FutureExt;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::Bus;
use crate::errors::{BoughError, ErrorKind};
use crate::harness::protocol::{HostFnName, ProgramResult};
use crate::harness::vm::{run_program, RunProgramOptions};
use crate::history::tags::echo::{create_command_echo, EchoCtx};
use crate::history::tags::record::{create_command_recorder, RecorderCtx};
use crate::history::tags::stats::{
    dir_tag_hints, drain_query_tag_hints, note_query_tag_hints, stats_memo, tags_note_for,
    SemanticRecall,
};
use crate::hooks::{Effect, HookDispatch, HookEvent};
use crate::hostfn::files::{create_file_host_fns, FileCtx};
use crate::hostfn::shell::{create_shell_host_fns, EchoHooks, ShellCtx, ShellOptions};
use crate::llm::pricing::context_window_for;
use crate::llm::retry::{RetryInfo, RetryOpts};
use crate::llm::routing::process_env;
use crate::llm::routing::{api_key_env, provider_for, Provider};
use crate::llm::trace::{trace_label, write_manifest, TurnManifest};
use crate::llm::{client_for, ClientOpts};
use crate::mcp::manager::McpGrant;
use crate::prompt::assemble::{
    assemble_prompt, scratch_note, workspace_note, AssembledPrompt, PromptInput, PromptMcpServer,
};
use crate::prompt::project::{note_project_rules, project_rules_note};
use crate::schema::events::{
    EventInput, EventType, MessageDeltaData, MessageFinishedData, MessagePartData,
    MessageRetryData, ToolLogData,
};
use crate::schema::parts::{Message, Part, Role, Session, SessionKind, Usage};
use crate::scratch::ensure_scratch_dir;
use crate::turn::queue::{
    abortable_delay, classify_round_failure, is_abort, short_reason, short_reason_text,
    should_drain, ClassifyOpts, TurnRegistry,
};
use crate::turn::replay::{build_thread, ThreadOptions};
use crate::turn::state::{checkpoint, finish_turn, start_turn, FinalTurnStatus, FinishOpts};
use crate::types::{
    AppCtx, Clock, Db, Effort, ExitNote, HostFns, LlmBlock, LlmClient, LlmContentBlock, LlmMessage,
    LlmParams, LlmRole, LlmToolDef, OnText, SharedDb, TurnCtx, TurnStarter,
};

// ---------------------------------------------------------------------------
// The model-facing surface
// ---------------------------------------------------------------------------

/// The entire model-facing API: two tools, and one of them is loop control.
///
/// A per-session or per-capability tool would split the provider's prompt
/// cache — tool definitions precede the system prompt in the cache order, so
/// one varying byte here costs every session the shared prefix. Capabilities
/// are granted through host functions inside `run_steps` and the prompt
/// sections that document them, never by adding a tool.
pub const RUN_STEPS: &str = "run_steps";
pub const STOP: &str = "stop";

/// The two tool definitions, byte-stable across rounds and sessions
/// (prompt-cache contract).
pub static TOOLS: LazyLock<Vec<LlmToolDef>> = LazyLock::new(|| {
    vec![
        LlmToolDef {
            name: RUN_STEPS.to_string(),
            description: "Run one JavaScript program in the workspace.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The program. Host functions are pre-injected globals.",
                    },
                    "done": {
                        "type": "boolean",
                        "description": "The work is complete after this program.",
                    },
                },
                "required": ["code"],
                "additionalProperties": false,
            }),
        },
        LlmToolDef {
            name: STOP.to_string(),
            description: "End the turn. Call after your final text, in the same response."
                .to_string(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
    ]
});

/// Validated at the boundary, like every other wire shape. A `code` that
/// arrived as a number is a model mistake the next round can fix; it must not
/// reach `run_program` as one.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunStepsInput {
    pub code: String,
    #[serde(default)]
    pub done: Option<bool>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The output reservation every round makes. The context meter measures
/// against it.
pub const MAX_TOKENS: i64 = 64_000;

/// Used when neither the ctx nor the session pins one.
pub const DEFAULT_MODEL: &str = "claude-opus-4-8";

/// Re-prompts before the harness stops waiting for an explicit `stop`.
pub const MAX_STOP_NUDGES: u32 = 3;

const STOP_NUDGE: &str = "[harness] Your turn is still open — it only ends when you call the stop \
     tool. Continue if there is more to do, or call stop now (alone, no other output) if you \
     are finished.";

/// Asks for a CLOSING report, not merely "some text".
///
/// The wording matters and was learned the hard way: "you have written no
/// user-visible text this turn" describes the mute case only, and an agent
/// that narrated on its way through would answer it with nothing, ending on a
/// raw tool dump with its last word being "Let me implement the changes:".
/// What the user needs at the end is the outcome, not the plan.
const REPORT_NUDGE: &str = "[harness] Your turn is about to end and the last thing the user \
     can see is tool output — anything you wrote earlier was narration of work in \
     progress, not a conclusion. Close the turn now: say what you changed (name the \
     files), what you verified and how it came out, and anything you did NOT do or \
     left uncertain. A few lines is plenty; do not restate your plan or re-explain \
     the code. Then call stop in the same response.";

/// A literal `<stop/>` ending the text, possibly repeated or padded.
///
/// Models sometimes *emit* the sentinel as text instead of calling the tool.
/// Parsed tolerantly: it is stripped from what gets stored (loop control, not
/// content — the same rule as the `stop` call) and honored as the stop it
/// meant. End-anchored on purpose, so prose that merely mentions the token in
/// a code span is never touched.
static TRAILING_STOP_SENTINEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\s*<stop\s*/>)+\s*$").unwrap());

/// The interrupt note. `⏹` and not `⚠︎`: the user asked for this.
const STOPPED_NOTE: &str = "⏹ Stopped.";

// ---------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------

/// One `run_steps` execution, as the runner asks for it.
pub struct ProgramRun {
    pub code: String,
    /// The `tool_use` id, so streamed lines are attributed to the right card.
    pub call_id: String,
    /// The turn's interrupt.
    pub cancel: CancellationToken,
    pub on_log: Arc<dyn Fn(&str) + Send + Sync>,
}

/// Executes one program. Injected so the whole loop is drivable with a fake
/// and no worker is ever spawned in a unit test — which is the difference
/// between a turn test that runs in milliseconds offline and one that needs a
/// machine.
pub type ProgramRunner = Arc<dyn Fn(ProgramRun) -> BoxFuture<'static, ProgramResult> + Send + Sync>;

/// The host functions the default runner bridges, and therefore the
/// capabilities the prompt grants. Shell and files are always wired;
/// everything else arrives with its milestone, and `deps.granted` is how a
/// caller that bridges more says so.
pub const BASE_HOST_FNS: [HostFnName; 9] = [
    HostFnName::Bash,
    HostFnName::Sh,
    HostFnName::BashBg,
    HostFnName::BashOutput,
    HostFnName::BashWait,
    HostFnName::BashKill,
    HostFnName::View,
    HostFnName::Patch,
    HostFnName::Write,
];

/// The per-turn command recorder over the turn's own seams — one vocabulary
/// read per repo per turn, absolute touched dirs pushed onto `ctx.touched`
/// (the dir-hint trigger), every failure swallowed.
fn recorder_for(ctx: &TurnCtx) -> crate::types::CommandRecorder {
    create_command_recorder(RecorderCtx {
        db: ctx.app.db.clone(),
        session_id: ctx.session_id.clone(),
        workspace: ctx.workspace.clone(),
        message_id: Some(ctx.message_id.clone()),
        now: Some(ctx.app.now.clone()),
        touched: Some(ctx.touched.clone()),
    })
}

/// The failure echo + loop guard, in the closure shape `ShellCtx` carries.
fn echo_hooks_for(ctx: &TurnCtx) -> EchoHooks {
    let echo = Arc::new(create_command_echo(EchoCtx {
        db: ctx.app.db.clone(),
        session_id: ctx.session_id.clone(),
        workspace: ctx.workspace.clone(),
        now: Some(ctx.app.now.clone()),
    }));
    let note_echo = echo.clone();
    EchoHooks {
        note: Arc::new(move |command, exit_code, output| {
            note_echo.note(command, exit_code, output)
        }),
        guard: Arc::new(move |command| echo.guard(command)),
    }
}

/// Build the always-wired host functions for one turn.
///
/// The shared trails live ON the ctx (`exits`, `reads`, `touched`) precisely
/// because host fns are built from it in more than one place — a
/// closure-local array shipped green tests while doing nothing live. Same
/// rule for the memory seams: `prepare_turn` puts ONE recorder on the ctx so
/// every construction path shares it; the fallback here covers a caller-built
/// ctx that never went through it.
pub fn base_host_fns(ctx: &TurnCtx) -> HostFns {
    let files = Arc::new(create_file_host_fns(
        FileCtx {
            workspace: ctx.workspace.clone(),
            session_id: ctx.session_id.clone(),
            reads: Some(ctx.reads.clone()),
        },
        ctx.app.host.snapshots.clone(),
        ctx.app.host.writes.clone(),
    ));

    let shell = Arc::new(create_shell_host_fns(
        ShellCtx {
            session_id: ctx.session_id.clone(),
            workspace: ctx.workspace.clone(),
            exits: Some(ctx.exits.clone()),
            refs: Some(ctx.round_refs.clone()),
            cancel: Some(ctx.cancel.clone()),
            scratch: Some(
                ensure_scratch_dir(&ctx.session_id)
                    .to_string_lossy()
                    .into_owned(),
            ),
            record: Some(ctx.record.clone().unwrap_or_else(|| recorder_for(ctx))),
            echo: Some(echo_hooks_for(ctx)),
            bus: Some(ctx.app.bus.clone()),
        },
        ShellOptions::new(ctx.app.host.jobs.clone()),
    ));

    let mut fns = HostFns::default();
    let s = shell.clone();
    fns.bash = Some(Arc::new(move |args: Vec<String>| {
        let s = s.clone();
        async move {
            s.bash(
                args.first().map(String::as_str).unwrap_or_default(),
                args.get(1).map(String::as_str),
            )
            .await
        }
        .boxed()
    }));
    let s = shell.clone();
    fns.sh = Some(Arc::new(move |args: Vec<String>| {
        let s = s.clone();
        async move { s.sh(args.first().map(String::as_str).unwrap_or("[]")).await }.boxed()
    }));
    let s = shell.clone();
    fns.bash_bg = Some(Arc::new(move |args: Vec<String>| {
        let s = s.clone();
        async move {
            s.bash_bg(
                args.first().map(String::as_str).unwrap_or_default(),
                args.get(1).map(String::as_str).unwrap_or_default(),
            )
        }
        .boxed()
    }));
    let s = shell.clone();
    fns.bash_output = Some(Arc::new(move |args: Vec<String>| {
        let s = s.clone();
        async move { s.bash_output(args.first().map(String::as_str).unwrap_or_default()) }.boxed()
    }));
    let s = shell.clone();
    fns.bash_wait = Some(Arc::new(move |args: Vec<String>| {
        let s = s.clone();
        async move {
            s.bash_wait(args.first().map(String::as_str).unwrap_or_default())
                .await
        }
        .boxed()
    }));
    let s = shell;
    fns.bash_kill = Some(Arc::new(move |args: Vec<String>| {
        let s = s.clone();
        async move {
            s.bash_kill(args.first().map(String::as_str).unwrap_or_default())
                .await
        }
        .boxed()
    }));
    let f = files.clone();
    fns.view = Some(Arc::new(move |args: Vec<String>| {
        let f = f.clone();
        async move { f.view(args.first().map(String::as_str).unwrap_or_default()) }.boxed()
    }));
    let f = files.clone();
    fns.patch = Some(Arc::new(move |args: Vec<String>| {
        let f = f.clone();
        async move { f.patch(args.first().map(String::as_str).unwrap_or_default()) }.boxed()
    }));
    let f = files;
    fns.write = Some(Arc::new(move |args: Vec<String>| {
        let f = f.clone();
        async move {
            f.write(
                args.first().map(String::as_str).unwrap_or_default(),
                args.get(1).map(String::as_str).unwrap_or_default(),
            )
        }
        .boxed()
    }));
    fns
}

/// The production program runner: a fresh worker per round with the turn's
/// host functions bridged and the turn's interrupt wired into the wind-down.
///
/// Reads the ctx trails **by index from a per-round snapshot** so a round
/// reports only its own exits, its own `view()` reads and its own touched
/// dirs. The exit notes, the per-directory tag hints and the project-rule
/// report all land on the round's RESULT, never the prompt — a mid-session
/// prompt edit would bust the volatile-tier cache (`llm/client`).
pub fn default_program_runner(ctx: &TurnCtx, host: Option<HostFns>) -> ProgramRunner {
    let fns = host.unwrap_or_else(|| base_host_fns(ctx));
    // Per TURN on the ctx, read per ROUND by index: the host functions may
    // have been built by the caller, so the arrays cannot live in this
    // closure.
    let exits = ctx.exits.clone();
    let reads = ctx.reads.clone();
    let touched = ctx.touched.clone();
    let round_refs = ctx.round_refs.clone();
    let hint_ctx = ctx.clone();
    // Resolved once per turn, from the same cache `prepare_turn` read for the
    // prompt — so the functions bound into the scope are the ones documented.
    let extension_files = crate::extensions::for_workspace(Path::new(&ctx.workspace))
        .files
        .clone();
    Arc::new(move |run: ProgramRun| {
        let extension_files = extension_files.clone();
        let fns = fns.clone();
        let exits = exits.clone();
        let reads = reads.clone();
        let touched = touched.clone();
        let round_refs = round_refs.clone();
        let hint_ctx = hint_ctx.clone();
        async move {
            let from = exits.lock().unwrap().len();
            let from_reads = reads.lock().unwrap().len();
            let from_touched = touched.lock().unwrap().len();
            let from_refs = round_refs.lock().unwrap().len();
            let on_log = run.on_log.clone();
            let result = run_program(RunProgramOptions {
                code: run.code,
                host: fns,
                timeout_ms: None,
                cancel: Some(run.cancel),
                on_log: Some(on_log),
                extensions: extension_files,
            })
            .await;
            let round_exits: Vec<ExitNote> = {
                let guard = exits.lock().unwrap();
                guard[from.min(guard.len())..].to_vec()
            };
            let round_dirs: Vec<String> = {
                let reads = reads.lock().unwrap();
                let touched = touched.lock().unwrap();
                reads[from_reads.min(reads.len())..]
                    .iter()
                    .filter_map(|p| {
                        Path::new(p)
                            .parent()
                            .map(|d| d.to_string_lossy().into_owned())
                    })
                    .chain(touched[from_touched.min(touched.len())..].iter().cloned())
                    .collect()
            };
            let touched_refs: Vec<(String, Option<i64>)> = {
                let guard = round_refs.lock().unwrap();
                guard[from_refs.min(guard.len())..].to_vec()
            };
            // DETACHED, never awaited. The fold writes `note_log` rows; the
            // hint below reads `note_sections`, so nothing the fold produces
            // can appear in this round's result — or any round's. Awaiting it
            // therefore bought nothing and put a cheap-model round trip (up to
            // CHEAP_TIMEOUT_MS) on the critical path of every round that
            // touched a reference, paid again on each round of a multi-round
            // turn. The one part of the note memory that could make a turn
            // WORSE, in a design whose whole contract is that a failure is a
            // non-event.
            spawn_note_fold(&hint_ctx, &touched_refs);
            with_note_hint_notes(
                with_project_rule_notes(
                    with_query_tag_hint_notes(
                        with_dir_tag_hint_notes(
                            with_exit_notes(result, &round_exits),
                            &hint_ctx,
                            &round_dirs,
                        ),
                        &hint_ctx,
                    ),
                    &hint_ctx,
                ),
                &hint_ctx,
                &touched_refs,
            )
        }
        .boxed()
    })
}

/// Append the `AGENTS.md` report queued when this turn's prompt was
/// assembled.
///
/// Same carrier as the tag hints and for the same reason — the round's
/// RESULT, not the prompt, because a per-turn prompt edit would bust the
/// volatile tier's cache. The queue drains on the first round of the turn, so
/// a multi-round turn says it once. `prompt/project.rs` owns what is worth
/// saying.
fn with_project_rule_notes(result: ProgramResult, ctx: &TurnCtx) -> ProgramResult {
    let lines = crate::prompt::project::drain_project_rule_notes(&ctx.session_id);
    if lines.is_empty() {
        return result;
    }
    let mut logs = result.logs.clone();
    logs.extend(lines);
    ProgramResult { logs, ..result }
}

/// Fire a turn-boundary event and apply what the hooks asked for.
///
/// EFFECTS ARE APPLIED HERE, not inside the interpreter: `post_system_note`
/// owns the wake rule (start a turn on an idle session, queue behind a running
/// one) and re-deciding it on the hook thread would be a second, subtly
/// different rule. An injected prompt is delivered as a system note for the
/// same reason `/schedules` and artifact comments are — it is input the
/// harness produced, and the transcript should say so rather than forge a
/// message the user did not type.
///
/// Context from a turn-boundary hook is announced the same way, because the
/// prompt is already assembled by the time this runs; a prompt edit here would
/// bust the volatile tier for the next turn (`prompt/assemble.rs`).
fn apply_turn_hooks(
    app: &AppCtx,
    session_id: &str,
    workspace: &str,
    event: HookEvent,
    data: serde_json::Value,
) {
    let Some(outcome) = crate::hooks::fire_on(
        Some(&app.bus),
        event,
        HookDispatch {
            session_id: session_id.to_string(),
            workspace: workspace.to_string(),
            pattern: session_id.to_string(),
            data,
        },
    ) else {
        return;
    };
    for text in outcome.context {
        // Deduplicated, because these are PERSISTED into the thread: a hook
        // that injects the same document every turn would otherwise grow the
        // conversation by a copy of it per turn, forever.
        let text = crate::hooks::dedupe::once_per_session(session_id, &text);
        crate::agents::notes::post_system_note(
            app,
            session_id,
            &format!("[hook] {text}"),
            &Default::default(),
        );
    }
    for effect in outcome.effects {
        match effect {
            Effect::Prompt { text } => {
                crate::agents::notes::post_system_note(
                    app,
                    session_id,
                    &format!("[hook] {text}"),
                    &Default::default(),
                );
            }
            Effect::SetTitle { title } => {
                let _ = with_db(&app.db, |d| d.set_session_title(session_id, &title));
            }
        }
    }
}

/// Append what this repo has already run on the topic the user named, queued
/// when the turn's prompt was built. Drains, so a multi-round turn says it on
/// the first round and never again.
fn with_query_tag_hint_notes(result: ProgramResult, ctx: &TurnCtx) -> ProgramResult {
    let lines = drain_query_tag_hints(stats_memo(), &ctx.session_id);
    if lines.is_empty() {
        return result;
    }
    let mut logs = result.logs.clone();
    logs.extend(lines);
    ProgramResult { logs, ..result }
}

/// Append the per-directory tag hints for directories this round newly
/// touched — by `view()` reads or by the paths its shell commands named —
/// the mid-turn half of the tag-history memory. Appended to the round's
/// RESULT, never to the prompt. `history/tags/stats.rs` owns per-dir repo
/// resolution, the divergence rule and the caps; the dirs here are absolute.
fn with_dir_tag_hint_notes(
    result: ProgramResult,
    ctx: &TurnCtx,
    abs_dirs: &[String],
) -> ProgramResult {
    let mut dirs: Vec<String> = Vec::new();
    for d in abs_dirs {
        if Path::new(d).is_absolute() && !dirs.contains(d) {
            dirs.push(d.clone());
        }
    }
    if dirs.is_empty() {
        return result;
    }
    let now = (ctx.app.now)();
    let lines = with_db(&ctx.app.db, |d| {
        dir_tag_hints(d, stats_memo(), &ctx.session_id, &ctx.workspace, &dirs, now)
    });
    if lines.is_empty() {
        return result;
    }
    let mut logs = result.logs.clone();
    logs.extend(lines);
    ProgramResult { logs, ..result }
}

/// Append what this repo's notes already say about the references this round
/// touched.
///
/// REFERENCE-TRIGGERED, not directory-triggered, unlike the tag hints beside
/// it. Measured on a real memory: 13.6% of commands carry a directory
/// attribution at all, and the ones that do not are concentrated in exactly
/// the infra work where a note is worth the most. Every command carries tags.
///
/// WHAT BOUNDS THE COST IS CHANGE, NOT A CAP. `resolve::hint_line` consults
/// the injection ledger, so a section already in this session's context above
/// produces nothing, one that grew produces only its new lines, and one that
/// was rewritten is re-sent whole and labelled. A stable note therefore costs
/// one line per session however many rounds touch it.
///
/// On the round's RESULT, like every other hint here, because a mid-turn
/// prompt edit would bust the volatile tier's cache.
fn with_note_hint_notes(
    result: ProgramResult,
    ctx: &TurnCtx,
    refs: &[(String, Option<i64>)],
) -> ProgramResult {
    if refs.is_empty() {
        return result;
    }
    let mut context: Vec<String> = Vec::new();
    for (tag, _) in refs {
        if !context.contains(tag) {
            context.push(tag.clone());
        }
    }
    let spread = with_db(&ctx.app.db, |d| d.tag_spread(None))
        .map(|(repos, by_tag)| crate::history::tags::stats::TagSpread { repos, by_tag })
        .unwrap_or_default();
    let sections =
        with_db(&ctx.app.db, |d| d.sections_for_context(&context, None)).unwrap_or_default();
    let ranked = crate::notes::resolve::rank(&spread, sections, &context, None);

    let lines: Vec<String> = ranked
        .iter()
        .filter_map(|r| crate::notes::resolve::hint_line(&ctx.session_id, r))
        .collect();
    if lines.is_empty() {
        return result;
    }
    let mut logs = result.logs.clone();
    logs.extend(lines);
    ProgramResult { logs, ..result }
}

/// Start the fold and return. Errors and panics inside it die with the task:
/// a lost note line is strictly better than a broken round, the same contract
/// the command recorder holds.
fn spawn_note_fold(ctx: &TurnCtx, refs: &[(String, Option<i64>)]) {
    if refs.is_empty() || ctx.app.cheap.is_none() {
        return;
    }
    let ctx = ctx.clone();
    let refs = refs.to_vec();
    tokio::spawn(async move {
        fold_round_into_notes(&ctx, &refs).await;
    });
}

/// Should the fold call the model for this reference right now?
///
/// ONE IN FLIGHT PER REFERENCE, AND ONE PER DEBOUNCE WINDOW — the discipline
/// the live activity blurbs already run under, and for the same reason: a turn
/// loops, so a ten-round turn working on one ticket would otherwise make ten
/// calls saying near-identical things. Dropping bounds the spend at one call
/// per reference per window AND keeps every line that does land true of
/// something recent, because the next round describes itself.
fn fold_is_due(tag: &str, now: i64) -> bool {
    static LAST: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, i64>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let Ok(mut guard) = LAST.lock() else {
        return false;
    };
    // Bounded: a long-lived server must not accumulate a row per reference it
    // has ever seen.
    if guard.len() > 512 {
        guard.retain(|_, at| now - *at < FOLD_DEBOUNCE_MS);
    }
    match guard.get(tag) {
        Some(at) if now - *at < FOLD_DEBOUNCE_MS => false,
        _ => {
            guard.insert(tag.to_string(), now);
            true
        }
    }
}

/// How long a reference rests between folds.
pub const FOLD_DEBOUNCE_MS: i64 = 10 * 60 * 1000;

/// The automatic write: one cheap-model line per reference this round touched.
///
/// EVERY GATE IS A NON-EVENT. No cheap tier, no note and no threshold met, a
/// SKIP, a provider that never answers — all leave the round exactly as it
/// was. Nothing here can fail a turn: the contract `worker/titles.rs` holds
/// for the whole tier.
///
/// The frontier advances only to the newest row actually folded, never to
/// `now` — a fold that skipped rows must not mark them accounted for.
async fn fold_round_into_notes(ctx: &TurnCtx, refs: &[(String, Option<i64>)]) {
    let Some(cheap) = ctx.app.cheap.clone() else {
        return;
    };
    if refs.is_empty() {
        return;
    }
    let repo = crate::history::tags::stats::workspace_repo(&ctx.workspace);
    let now = (ctx.app.now)();

    let mut seen: Vec<String> = Vec::new();
    for (tag, exit_code) in refs {
        if seen.contains(tag) {
            continue;
        }
        seen.push(tag.clone());
        if !fold_is_due(tag, now) {
            continue;
        }

        let existing = with_db(&ctx.app.db, |d| d.note_by_path(tag)).ok().flatten();
        let note = match existing {
            Some(note) => note,
            None => {
                // A page is created only for a reference that has EARNED one.
                // The alternative is a page per reference: 143 of them on a
                // real memory, and an index nobody can read.
                let probe = crate::types::NoteRow {
                    id: 0,
                    path: tag.clone(),
                    title: tag.clone(),
                    tags: vec![tag.clone()],
                    created_at: now,
                    updated_at: now,
                    synced_ts: 0,
                    closed_at: None,
                };
                let drift = with_db(&ctx.app.db, |d| {
                    crate::notes::drift_for(d, &probe, Some(&repo), false)
                });
                if !crate::notes::earns_a_page(&drift) {
                    continue;
                }
                let Ok(id) = with_db(&ctx.app.db, |d| {
                    d.upsert_note(tag, tag, std::slice::from_ref(tag), now)
                }) else {
                    continue;
                };
                crate::types::NoteRow { id, ..probe }
            }
        };

        let sections = with_db(&ctx.app.db, |d| d.sections_for_note(note.id)).unwrap_or_default();
        let claim = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let log = with_db(&ctx.app.db, |d| d.note_log(note.id, 20)).unwrap_or_default();

        let prompt = crate::worker::notes::round_gist(
            &note,
            &claim,
            &log,
            &[crate::worker::notes::RoundTag {
                tag: tag.clone(),
                exit_code: *exit_code,
            }],
            "",
        );
        let Some(line) = cheap.note_line(&prompt).await else {
            continue;
        };

        // Best-effort, exactly like the command recorder: a lost note line is
        // strictly better than a broken round.
        let wrote = with_db(&ctx.app.db, |d| {
            d.append_note_log(note.id, now, crate::types::NoteAuthor::Cheap, &line)
        })
        .unwrap_or(false);
        if !wrote {
            continue;
        }
        let drift = with_db(&ctx.app.db, |d| {
            crate::notes::drift_for(d, &note, Some(&repo), false)
        });
        if let Some(ts) = drift.newest_ts {
            let _ = with_db(&ctx.app.db, |d| d.set_note_synced(note.id, ts));
        }
    }
}

/// Append the commands that exited non-zero, when the program did not print
/// them itself.
///
/// `bash()` returns `[exit code N]` as data rather than throwing, which is the
/// right call — it is a result to read. But the string goes into the program,
/// so a round that never logs it leaves the failure INVISIBLE: a reviewer ran
/// `await bash("exit 3")` without logging and got `◇ run_steps ✓ done` over
/// "(the program ran and printed nothing)", after which the model narrated a
/// confident invented mechanism ("bash() threw on the non-zero exit"). The
/// harness knew the code all along.
///
/// Only when it is not already there: a program that DID log the output has
/// said it, and saying it twice is its own kind of noise.
pub fn with_exit_notes(result: ProgramResult, exits: &[ExitNote]) -> ProgramResult {
    if exits.is_empty() {
        return result;
    }
    let said = result.logs.join("\n");
    let unsaid: Vec<&ExitNote> = exits
        .iter()
        .filter(|e| !said.contains(&format!("[exit code {}", e.code)))
        .collect();
    if unsaid.is_empty() {
        return result;
    }
    let mut logs = result.logs.clone();
    logs.extend(
        unsaid
            .iter()
            .map(|e| format!("[exit code {}] {}", e.code, one_line_command(&e.command))),
    );
    ProgramResult { logs, ..result }
}

/// A command on one line, short enough to sit in a result.
fn one_line_command(command: &str) -> String {
    let flat = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 80 {
        format!("{}…", flat.chars().take(79).collect::<String>())
    } else {
        flat
    }
}

// ---------------------------------------------------------------------------
// Deps and outcomes
// ---------------------------------------------------------------------------

/// Where a failed turn's raw error is reported. The default logs it, because
/// the UI must never know more than the server log does.
pub type ReportError = Arc<dyn Fn(&BoughError, &str) + Send + Sync>;

/// All injection points, mirroring the TS `TurnDeps`.
#[derive(Clone, Default)]
pub struct TurnDeps {
    /// Defaults to the ctx's registry. Tests pass their own.
    pub registry: Option<Arc<TurnRegistry>>,
    /// A fixed program runner — what a test passes, since a fake needs nothing
    /// from the turn. Wins over `program_for`.
    pub program: Option<ProgramRunner>,
    /// A runner built from the turn's own ctx (its workspace, its interrupt).
    /// This is the shape production needs and the reason the two are separate
    /// fields: both are one-argument functions, so a single field could not
    /// tell them apart. Defaults to [`default_program_runner`].
    pub program_for: Option<Arc<dyn Fn(&TurnCtx) -> ProgramRunner + Send + Sync>>,
    /// Defaults to [`assemble_prompt`].
    pub assemble: Option<Arc<dyn Fn(&PromptInput) -> AssembledPrompt + Send + Sync>>,
    /// Host functions this turn bridges, for the prompt's capability gating.
    pub granted: Option<Vec<HostFnName>>,
    /// Extra volatile prompt notes (project rules, running subagents).
    pub notes: Option<Vec<String>>,
    /// Injected clock. Absent = `ctx.now`.
    pub now: Option<Clock>,
    /// Round-retry knobs; tests turn the outage delay down so a test is not a
    /// minute.
    pub max_round_retries: Option<u32>,
    pub outage_delay_ms: Option<u64>,
    pub max_tokens: Option<i64>,
    /// Background shells that outlive an interrupt, so the stop note can name
    /// them (they are detached on purpose). Absent = say nothing rather than
    /// claim there were none.
    pub surviving_jobs: Option<Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>>,
    /// Recursion seam: how a queued drain starts the next turn. Tests observe
    /// it.
    pub start_next: Option<Arc<dyn Fn(&AppCtx, &str) + Send + Sync>>,
    /// A test passes a collector so an intentional failure does not print a
    /// stack, and so the reporting can be asserted rather than inferred.
    pub report_error: Option<ReportError>,
    /// Bind this turn's MCP grant as a LIVE read of its session's activations
    /// (`mcp::manager::bind_turn_grant`). Boot sets it; a test leaves it false
    /// so nothing reads the real `~/.bough/mcp.json`. Ignored when
    /// `mcp_grant` carries an inherited snapshot — a subagent's grant is its
    /// spawner's and re-deriving it from the child's own (empty) activations
    /// would revoke it.
    pub bind_mcp_grant: bool,
    /// The spawn-time snapshot a subagent's turn inherits (`agents/subagent`).
    pub mcp_grant: Option<Vec<String>>,
    /// The turn-start MCP catalog, per session — the same read-only status
    /// document the panel and `bough mcp` render. Resolved by the caller
    /// because only boot knows the session at this seam; absent = no catalog,
    /// and the prompt's MCP section simply does not render.
    /// `(session_id, servers the turn's skills grant on top of the session's
    /// own activations)`.
    pub mcp_catalog: Option<Arc<dyn Fn(&str, &[String]) -> Vec<PromptMcpServer> + Send + Sync>>,
}

/// How a finished turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnOutcomeStatus {
    Done,
    Error,
    Interrupted,
}

impl TurnOutcomeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TurnOutcomeStatus::Done => "done",
            TurnOutcomeStatus::Error => "error",
            TurnOutcomeStatus::Interrupted => "interrupted",
        }
    }
}

impl From<TurnOutcomeStatus> for FinalTurnStatus {
    fn from(s: TurnOutcomeStatus) -> FinalTurnStatus {
        match s {
            TurnOutcomeStatus::Done => FinalTurnStatus::Done,
            TurnOutcomeStatus::Error => FinalTurnStatus::Error,
            TurnOutcomeStatus::Interrupted => FinalTurnStatus::Interrupted,
        }
    }
}

/// What a finished turn reports to a caller that awaited it.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnOutcome {
    pub turn_id: String,
    pub message_id: String,
    pub status: TurnOutcomeStatus,
    pub error: Option<String>,
    pub usage: Usage,
}

/// What [`begin_turn`] hands back: the announced placeholder and the handle
/// that resolves when the turn is fully finished. The inner `Result` is the
/// TS "rejected before the loop's own catch" path — a failure in the few
/// statements before the loop (opening the turn row, reading the session).
pub struct StartedTurn {
    pub message: Message,
    pub done: tokio::task::JoinHandle<Result<TurnOutcome, BoughError>>,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Start a turn. Returns the pending supervisor message immediately and the
/// handle that resolves when the turn is fully finished.
///
/// The two are separate because the HTTP path is a 202 that discards the
/// handle — the turn outlives the response by minutes — while a test awaits
/// it. The message is created and announced **inline, before the loop is
/// spawned**, so a client that reconciles by id sees it even if the turn
/// finishes before the post returns.
///
/// The claim comes FIRST. `begin` errors when a turn is already running, and
/// it has to error before the placeholder exists — a message created and
/// announced and then abandoned would sit `pending` in the transcript with no
/// turn to close it, which is the exact hang this module is about.
pub fn begin_turn(
    ctx: &AppCtx,
    session_id: &str,
    deps: TurnDeps,
) -> Result<StartedTurn, BoughError> {
    let now: Clock = deps.now.clone().unwrap_or_else(|| ctx.now.clone());
    let registry = deps
        .registry
        .clone()
        .unwrap_or_else(|| ctx.turn_registry.clone());

    let claim = registry.begin(session_id)?;

    let message = Message {
        id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        role: Role::Supervisor,
        parts: vec![],
        pending: true,
        created_at: now(),
    };
    if let Err(err) = with_db(&ctx.db, |d| d.create_message(message.clone())) {
        // The claim must not outlive a turn that never started.
        registry.end(&claim);
        return Err(err);
    }
    ctx.bus.publish(event(
        EventType::MessageStarted,
        session_id,
        serde_json::to_value(&message).unwrap_or_default(),
    ));

    // Setup runs INLINE — the TS drive is synchronous up to its first await,
    // which covers everything through `startTurn`. A caller returning from
    // `begin_turn` must already see the `running` turn row (`busy_session_ids`
    // reads it) and the announced message; only the loop is spawned.
    let prepared = prepare_turn(ctx, &message, claim.cancel.clone(), deps.clone());

    let ctx2 = ctx.clone();
    let deps2 = deps;
    let sid = session_id.to_string();
    let done = tokio::spawn(async move {
        let result = match prepared {
            Ok(p) => match AssertUnwindSafe(drive(p)).catch_unwind().await {
                Ok(outcome) => outcome,
                Err(panic) => Err(BoughError::http(
                    500,
                    ErrorKind::Turn,
                    format!("turn panicked: {}", panic_text(&panic)),
                )),
            },
            // A failure before the loop (opening the turn row, reading the
            // session) — the TS "rejected before drive's try" path.
            Err(err) => Err(err),
        };

        // The epilogue — on every path, panics included. Release first, drain
        // second: `begin` would throw on a session this turn had not let go of
        // yet.
        registry.end(&claim);
        let drain = with_db(&ctx2.db, |d| should_drain(d, &sid, &registry)).unwrap_or(false);
        if drain {
            // A message that landed mid-turn becomes a fresh turn now.
            match &deps2.start_next {
                Some(next) => next(&ctx2, &sid),
                None => start_detached(&ctx2, &sid, deps2.clone()),
            }
        }

        result
    });

    Ok(StartedTurn { message, done })
}

/// Start a turn nobody will await.
///
/// The error handling is not politeness: `drive` handles its own failures, but
/// the few statements before its catch region (opening the turn row, reading
/// the session) can still fail, and an unreported failure here silently
/// strands the message.
pub fn start_detached(ctx: &AppCtx, session_id: &str, deps: TurnDeps) {
    let report = deps.report_error.clone();
    let sid = session_id.to_string();
    match begin_turn(ctx, session_id, deps) {
        Ok(started) => {
            tokio::spawn(async move {
                match started.done.await {
                    Ok(Ok(_)) | Err(_) => {}
                    Ok(Err(err)) => report_failure(&report, &err, &sid),
                }
            });
        }
        Err(err) => report_failure(&report, &err, &sid),
    }
}

fn report_failure(report: &Option<ReportError>, err: &BoughError, session_id: &str) {
    match report {
        Some(f) => f(err, session_id),
        None => tracing::error!("turn failed to start [{session_id}]: {err}"),
    }
}

/// The `TurnStarter` the server reads off the ctx.
///
/// A post into a session that is already busy never reaches here — the handler
/// checks `busy_session_ids()` and 202s — but the guard is repeated anyway:
/// the registry is the authority on "one turn per session", and a second
/// caller (a schedule firing, a system note waking a session) must hit the
/// same wall.
pub fn create_turn_starter(deps: TurnDeps) -> Arc<dyn TurnStarter> {
    Arc::new(StarterImpl { deps })
}

struct StarterImpl {
    deps: TurnDeps,
}

impl TurnStarter for StarterImpl {
    fn start_turn(&self, ctx: &AppCtx, session: &Session, _message: &Message) {
        let registry = self
            .deps
            .registry
            .clone()
            .unwrap_or_else(|| ctx.turn_registry.clone());
        if registry.is_running(&session.id) {
            registry.enqueue(&session.id);
            return;
        }
        start_detached(ctx, &session.id, self.deps.clone());
    }
}

/// Stop the session's turn and cascade to its detached children.
pub fn interrupt_turn(session_id: &str, registry: &TurnRegistry) -> bool {
    registry.interrupt(session_id)
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// How the loop unwinds. `drive` never lets either variant escape — every
/// failure becomes a closed message and a [`TurnOutcome`], exactly as TS does.
enum TurnFailure {
    Interrupted,
    Failed(BoughError),
}

impl From<BoughError> for TurnFailure {
    fn from(e: BoughError) -> Self {
        TurnFailure::Failed(e)
    }
}

/// Everything the loop needs, resolved synchronously by [`prepare_turn`]
/// before the drive task is spawned.
struct PreparedTurn {
    db: SharedDb,
    bus: Arc<Bus>,
    /// The checkout this turn edits, carried for the turn-boundary hooks.
    workspace: String,
    /// Carried so the turn-boundary hooks can post an injected prompt through
    /// the same starter a user message uses (`agents/notes.rs`).
    app: AppCtx,
    session_id: String,
    message_id: String,
    now: Clock,
    max_tokens: i64,
    delegated: bool,
    model: String,
    effort: Option<Effort>,
    turn: crate::schema::parts::Turn,
    program: ProgramRunner,
    llm: Arc<dyn LlmClient>,
    prompt: AssembledPrompt,
    messages: Vec<LlmMessage>,
    cancel: CancellationToken,
    deps: TurnDeps,
}

/// The synchronous head of the TS `drive`: resolve config, open the turn row,
/// build the ctx, the client, the prompt and the thread. Runs inline in
/// [`begin_turn`], before anything is spawned.
fn prepare_turn(
    ctx: &AppCtx,
    message: &Message,
    cancel: CancellationToken,
    deps: TurnDeps,
) -> Result<PreparedTurn, BoughError> {
    let db = ctx.db.clone();
    let bus = ctx.bus.clone();
    // Cloned for the hook dispatch at the end of the turn, which runs after
    // `ctx` itself is long out of scope.
    let app = ctx.clone();
    let session_id = message.session_id.clone();
    let message_id = message.id.clone();
    let now: Clock = deps.now.clone().unwrap_or_else(|| ctx.now.clone());
    let max_tokens = deps.max_tokens.unwrap_or(MAX_TOKENS);

    let session: Option<Session> = with_db(&db, |d| d.get_session(&session_id))?;
    // Session pin first, then the global default, then the built-in: `model`
    // and `effort` are per-session OVERRIDES. The ctx carries the GLOBAL
    // default, so reading it first would make `set_session_model` a no-op on
    // any install that sets `BOUGH_MODEL`. Same order for `effort` — one rule.
    let model = session
        .as_ref()
        .and_then(|s| s.model.clone())
        .or_else(|| ctx.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let effort: Option<Effort> = session
        .as_ref()
        .and_then(|s| s.effort.as_deref().and_then(parse_effort))
        .or(ctx.effort);
    let workspace = with_db(&db, |d| d.get_session_runtime(&session_id))?
        .workspace
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string())
        });
    // The session's scratchpad, made before the prompt names it.
    let scratch = ensure_scratch_dir(&session_id);

    let turn = with_db(&db, |d| start_turn(d, &session_id, &message_id, &now))?;

    let depth = if matches!(
        session.as_ref().map(|s| s.kind),
        Some(SessionKind::Subagent) | Some(SessionKind::WorkflowAgent)
    ) {
        1
    } else {
        0
    };
    let delegated = depth == 1;

    let mut turn_ctx = TurnCtx {
        app: ctx.clone(),
        session_id: session_id.clone(),
        turn_id: turn.id.clone(),
        message_id: message_id.clone(),
        workspace: workspace.clone(),
        model: model.clone(),
        cancel: cancel.clone(),
        exits: Arc::new(std::sync::Mutex::new(Vec::new())),
        record: None,
        reads: Arc::new(std::sync::Mutex::new(Vec::new())),
        touched: Arc::new(std::sync::Mutex::new(Vec::new())),
        round_refs: Arc::new(std::sync::Mutex::new(Vec::new())),
        // The grant this turn holds: an inherited snapshot when a spawner
        // handed one down, otherwise a LIVE read of this session's
        // activations when boot asked for one (so a revocation is visible to
        // the very next call), otherwise nothing.
        mcp_grant: match (&deps.mcp_grant, deps.bind_mcp_grant) {
            (Some(names), _) => Some(McpGrant::Inherited(names.clone())),
            (None, true) => Some(McpGrant::Live {
                session_id: session_id.clone(),
            }),
            (None, false) => None,
        },
        depth,
    };
    // ONE recorder per turn, shared by every construction path (the TS
    // `ctx.record ??=` rule): the vocabulary a turn is judged against must be
    // stable across the turn, and the touch trail must exist before the
    // recorder closes over it.
    turn_ctx.record = Some(recorder_for(&turn_ctx));

    let program: ProgramRunner = match (&deps.program, &deps.program_for) {
        (Some(p), _) => p.clone(),
        (None, Some(f)) => f(&turn_ctx),
        (None, None) => default_program_runner(&turn_ctx, None),
    };

    // The injected `ctx.llm` (tests) bypasses the retry wiring; production
    // composes the routed client with `message.retry` announcements — the
    // provider client's own backoff is invisible otherwise, and a retried
    // round re-streams from the top so the client also has to drop its
    // partial text. Tracing is `None` unless `BOUGH_TRACE_DIR` is set —
    // resolved here because this is the only place that knows both ids, and
    // written to below once the prompt is assembled (`llm/trace.rs`).
    let trace = trace_label(&session_id, &turn.id, &process_env());
    let llm: Arc<dyn LlmClient> = ctx.llm.clone().unwrap_or_else(|| {
        let bus = bus.clone();
        let sid = session_id.clone();
        let mid = message_id.clone();
        client_for(
            &model,
            ClientOpts {
                retry: RetryOpts {
                    on_retry: Some(Arc::new(move |info: RetryInfo| {
                        bus.publish(event(
                            EventType::MessageRetry,
                            &sid,
                            serde_json::to_value(MessageRetryData {
                                message_id: mid.clone(),
                                attempt: info.attempt,
                                reason: format!(
                                    "{} — retry {}/{}",
                                    short_reason(&info.error),
                                    info.attempt,
                                    info.max_attempts
                                ),
                            })
                            .unwrap_or_default(),
                        ));
                    })),
                    ..Default::default()
                },
                trace: trace.clone(),
                ..Default::default()
            },
        )
    });

    // The workspace note leads, unconditionally and for every kind. It is not
    // a capability grant, so it has no `granted` gate: a subagent, a workflow
    // agent and a schedule-fired root all edit a real checkout and all need to
    // be told which one. It is built HERE because `workspace` is resolved
    // here, per session — boot cannot supply a per-session fact.
    // Across the workspace AND every directory this session has actually run a
    // command in: a session opened in `$HOME` that works inside one repo has to
    // pick up that repo's AGENTS.md/CLAUDE.md, and the upward walk alone can
    // never reach below the workspace to find it.
    let rule_files = crate::prompt::project::session_rule_files(Path::new(&workspace), &session_id);
    let rules_note = project_rules_note(&rule_files, Path::new(&workspace));
    // What went in is reported, from the SAME read the prompt was built from —
    // drained onto the round's result by `with_project_rule_notes`.
    note_project_rules(&session_id, &rule_files, Path::new(&workspace));
    // Frozen per session even though this runs per turn — the memo in
    // `history/tags/stats.rs` — because the volatile tier caches per session
    // and a note whose text drifts mid-session would bust it. None for a
    // project with no command history yet, and then simply omitted.
    let tags_note = with_db(&db, |d| {
        tags_note_for(d, stats_memo(), &session_id, &workspace, now())
    });
    // The other half of the memory, matched on what the user actually asked
    // rather than on where they are. Computed here because the message is in
    // hand here; DELIVERED on the round's result, like the directory hints —
    // a per-turn prompt edit would bust the volatile tier.
    with_db(&db, |d| {
        let text = d
            .messages_for(&session_id)
            .map(|m| crate::skills::invoking_text(&m))
            .unwrap_or_default();
        note_query_tag_hints(
            d,
            stats_memo(),
            &session_id,
            &workspace,
            &text,
            now(),
            crate::history::tags::embed::recall_layer().map(|l| l as &dyn SemanticRecall),
        );
    });
    // TurnStart, before the prompt is assembled — the one event whose context
    // can still ride the prompt itself. It is the only per-turn text allowed
    // in the volatile tier, and only when a hook actually returns some: a
    // TurnStart hook that adds context is opting into a cache miss per turn,
    // which is the honest price of context the model sees BEFORE it acts
    // rather than after its first round.
    let start_hooks = crate::hooks::fire_on(
        Some(&bus),
        HookEvent::TurnStart,
        HookDispatch {
            session_id: session_id.clone(),
            workspace: workspace.clone(),
            pattern: session_id.clone(),
            data: serde_json::json!({
                "prompt": crate::skills::invoking_text(std::slice::from_ref(message)),
                "model": model,
            }),
        },
    );
    if let Some(stop) = start_hooks.as_ref().and_then(|o| o.stop.clone()) {
        // A hook that stops a turn stops it BEFORE the provider is called, so
        // the refusal costs nothing and reads as one in the transcript.
        return Err(BoughError::program(format!(
            "a hook stopped this turn: {stop}"
        )));
    }
    let mut notes: Vec<String> = vec![
        workspace_note(&workspace),
        scratch_note(&scratch.to_string_lossy()),
    ];
    if let Some(outcome) = &start_hooks {
        // WITHIN the turn, not across it. This prompt is rebuilt every turn,
        // so a cross-turn reference would name bytes that are no longer in
        // the window — the repetition is the cheaper wrong answer, and the
        // prompt cache absorbs an unchanged block anyway.
        for text in crate::hooks::dedupe::within_batch(&outcome.context) {
            notes.push(format!("## From a hook\n{text}"));
        }
    }
    if let Some(n) = tags_note {
        notes.push(n);
    }
    if let Some(n) = rules_note {
        notes.push(n);
    }
    if let Some(extra) = &deps.notes {
        notes.extend(extra.iter().cloned());
    }
    // The skills this turn's message named. Resolved HERE because this is
    // where the workspace is known and where the prompt is built — and until
    // this call existed, `PromptInput.skills` was hardcoded empty, so a `/name`
    // parsed, listed in the panel, and reached the model as nothing at all.
    let skill_sources = crate::skills::sources_for(Path::new(&workspace));
    let active = with_db(&db, |d| {
        crate::skills::turn_skills(d, &session_id, &skill_sources).unwrap_or_default()
    });
    // What EXISTS, one line each. Named skills are excluded because their
    // bodies are already below — listing a loaded skill as something to go
    // and open invites the model to read a file it is holding.
    let skill_catalog = crate::skills::catalog(&skill_sources, &active.names);
    // A named-and-broken skill tells the model why, rather than nothing
    // (`skills/mod.rs`'s intact-or-reported invariant). These are volatile,
    // like every other note here.
    notes.extend(active.notes.iter().cloned());
    // The workspace's extensions, probed once per edit and cached. Read HERE
    // for the same reason skills are: this is where the workspace is known.
    // The list feeds the prompt below and the program's scope in
    // `default_program_runner` — one list, both halves, so a documented
    // function is a bound one (`crate::extensions`).
    let extensions = crate::extensions::for_workspace(Path::new(&workspace));
    let prompt_input = PromptInput {
        kind: session
            .as_ref()
            .map(|s| s.kind)
            .unwrap_or(SessionKind::Root),
        granted: deps
            .granted
            .clone()
            .unwrap_or_else(|| BASE_HOST_FNS.to_vec()),
        // The MCP catalog, per turn, read fresh because grants and connections
        // change between turns. A capability nobody is told about is not a
        // capability: without this the mcp-tools section never renders and the
        // model never learns a granted server exists.
        // The catalog is widened by the turn's skills: a skill's `mcp:` list
        // IS its capability grant (spec §16), so a `/linear` that arrives
        // ungranted must still see the server in this prompt or the grant is
        // one the model is never told about.
        mcp_servers: deps
            .mcp_catalog
            .as_ref()
            .map(|catalog| catalog(&session_id, &active.servers))
            .unwrap_or_default(),
        extensions: extensions.fns.clone(),
        skills: active.skills.clone(),
        skill_catalog,
        notes,
    };
    let prompt: AssembledPrompt = match &deps.assemble {
        Some(f) => f(&prompt_input),
        None => assemble_prompt(&prompt_input),
    };

    // What this turn actually sent, for the surface that answers "what is in
    // my context window". Unconditional, unlike the trace below: this costs
    // one clone of a short list per turn, and a diagnostic nobody can reach
    // without setting an environment variable first is a diagnostic that is
    // never there when it is needed.
    crate::prompt::last::remember(&session_id, &prompt);

    // The section identities the raw trace cannot see: `LlmParams` carries
    // the assembled prefix as one opaque string, so which .md files went into
    // it has to be recorded from here (`llm/trace.rs`).
    if let Some(trace) = &trace {
        write_manifest(
            trace,
            &TurnManifest {
                session_id: session_id.clone(),
                turn_id: turn.id.clone(),
                model: model.clone(),
                effort: effort
                    .and_then(|e| serde_json::to_value(e).ok())
                    .and_then(|v| v.as_str().map(String::from)),
                workspace: Some(workspace.clone()),
                sections: prompt.shas.clone(),
                started_at: now(),
            },
        );
    }

    // The thread as the provider sees it, minus the message we are writing.
    // Built once: a turn's history does not change under it, and rebuilding
    // per round would re-read every attachment from disk every round.
    let thread = with_db(&db, |d| d.thread_for(&session_id))?;
    let messages: Vec<LlmMessage> = build_thread(
        &thread,
        &ThreadOptions {
            exclude: Some(&message_id),
            // Reasoning replays only to the model that signed it (replay.rs).
            model: Some(&model),
            load_image: None,
        },
    );
    drop(thread);

    Ok(PreparedTurn {
        app,
        workspace: workspace.clone(),
        db,
        bus,
        session_id,
        message_id,
        now,
        max_tokens,
        delegated,
        model,
        effort,
        turn,
        program,
        llm,
        prompt,
        messages,
        cancel,
        deps,
    })
}

/// The loop and its epilogues — the async tail of the TS `drive`. Never
/// propagates a loop failure: every one becomes a closed message and a
/// [`TurnOutcome`]. The `Err` arm is reserved for the machinery around the
/// loop (a db write in an epilogue).
async fn drive(p: PreparedTurn) -> Result<TurnOutcome, BoughError> {
    let PreparedTurn {
        db,
        bus,
        workspace,
        app,
        session_id,
        message_id,
        now,
        max_tokens,
        delegated,
        model,
        effort,
        turn,
        program,
        llm,
        prompt,
        mut messages,
        cancel,
        deps,
    } = p;

    /* The turn's running usage total. Replaces the row's each checkpoint. */
    let mut usage = Usage {
        input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: Some(0),
        cache_read_tokens: Some(0),
        cache_write_tokens: Some(0),
        cost_usd: Some(0.0),
    };
    /* Last round's prompt size — a gauge, not a total. Drives the overflow check. */
    let mut context_tokens: i64 = 0;

    let mut parts: Vec<Part> = Vec::new();

    let on_text: OnText = {
        let bus = bus.clone();
        let sid = session_id.clone();
        let mid = message_id.clone();
        Arc::new(move |delta: &str| {
            bus.publish(event(
                EventType::MessageDelta,
                &sid,
                serde_json::to_value(MessageDeltaData {
                    message_id: mid.clone(),
                    delta: delta.to_string(),
                })
                .unwrap_or_default(),
            ));
        })
    };

    let mut nudges: u32 = 0;
    let mut report_nudges: u32 = 0;
    /* The last-resort text-only round. Whatever it says is the turn's last word. */
    let mut force_text = false;

    // The loop, with panics folded into the error path: the TS catch-all maps
    // any throw to a finished-with-error turn.
    let loop_result: Result<(), TurnFailure> = {
        let fut = async {
            let mut round: u32 = 0;
            loop {
                if cancel.is_cancelled() {
                    return Err(TurnFailure::Interrupted);
                }

                // Checked before the request, not after the rejection: a round
                // that cannot fit is a turn error naming the limit, and
                // sending it anyway would spend the tokens to be told so in
                // provider dialect. Compaction is the user's move to make —
                // the harness never summarizes a conversation out from under
                // them and never auto-compacts mid-turn.
                if let Some(limit) = usable_context_limit(&model, max_tokens) {
                    if context_tokens > limit {
                        let window = context_window_for(&model).unwrap_or(0);
                        return Err(TurnFailure::Failed(BoughError::context_overflow(format!(
                            "this turn no longer fits in {model}'s context window: the last \
                             round's prompt was {} tokens against a usable limit of {} ({} \
                             window minus the {}-token output reservation). Compact or fork \
                             this session to continue — nothing was summarized automatically.",
                            locale(context_tokens),
                            locale(limit),
                            locale(window),
                            locale(max_tokens),
                        ))));
                    }
                }

                let params = LlmParams {
                    model: model.clone(),
                    system: Some(prompt.system.clone()),
                    system_volatile: Some(prompt.system_volatile.clone()),
                    max_tokens,
                    messages: messages.clone(),
                    tools: TOOLS.clone(),
                    tool_choice_none: force_text,
                    effort,
                };
                let result = run_round(
                    &llm,
                    params,
                    on_text.clone(),
                    &cancel,
                    &bus,
                    &session_id,
                    &message_id,
                    &deps,
                )
                .await?;

                if let Some(round_usage) = &result.usage {
                    context_tokens = round_usage.input_tokens
                        + round_usage.cache_read_tokens.unwrap_or(0)
                        + round_usage.cache_write_tokens.unwrap_or(0);
                    fold_usage(&mut usage, round_usage);
                    with_db(&db, |d| {
                        d.add_session_usage(&session_id, round_usage, now())
                    })?;
                    if let Some(refreshed) = with_db(&db, |d| d.get_session(&session_id))? {
                        bus.publish(event(
                            EventType::SessionUpdated,
                            &session_id,
                            serde_json::to_value(refreshed).unwrap_or_default(),
                        ));
                    }
                }

                // Persist what the round said, and build the in-memory echo.
                // These diverge in exactly two places, and both are
                // deliberate: `stop` is loop control and is never persisted or
                // replayed, and reasoning goes into the echo WITH its provider
                // meta while being persisted with the signing model as well.
                let mut stop_requested = false;
                let mut assistant: Vec<LlmContentBlock> = Vec::new();
                for block in &result.content {
                    match block {
                        LlmBlock::Text { text } => {
                            let mut text = text.clone();
                            if TRAILING_STOP_SENTINEL.is_match(&text) {
                                stop_requested = true;
                                text = TRAILING_STOP_SENTINEL.replace(&text, "").into_owned();
                            }
                            if !text.is_empty() {
                                append_part(
                                    &db,
                                    &bus,
                                    &session_id,
                                    &message_id,
                                    &mut parts,
                                    Part::Text { text: text.clone() },
                                )?;
                                assistant.push(LlmContentBlock::Text { text });
                            }
                        }
                        LlmBlock::Reasoning { text, meta } => {
                            // Persisted WITH its provider payload and the
                            // model that signed it, so the next turn can
                            // replay it (replay.rs, invariant 1). A block with
                            // no displayable text still persists when it is
                            // signed — that is a redacted thinking block, and
                            // it has to go back whole or not at all.
                            if !text.trim().is_empty() || meta.is_some() {
                                append_part(
                                    &db,
                                    &bus,
                                    &session_id,
                                    &message_id,
                                    &mut parts,
                                    Part::Reasoning {
                                        text: text.clone(),
                                        meta: meta.clone(),
                                        model: Some(model.clone()),
                                    },
                                )?;
                            }
                            assistant.push(LlmContentBlock::Reasoning {
                                text: text.clone(),
                                meta: meta.clone(),
                            });
                        }
                        LlmBlock::ToolUse { id, name, input } => {
                            if name == STOP {
                                stop_requested = true;
                                continue;
                            }
                            append_part(
                                &db,
                                &bus,
                                &session_id,
                                &message_id,
                                &mut parts,
                                Part::ToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: input.clone(),
                                },
                            )?;
                            assistant.push(LlmContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            });
                        }
                    }
                }
                if !assistant.is_empty() {
                    messages.push(LlmMessage {
                        role: LlmRole::Assistant,
                        content: assistant,
                    });
                }
                with_db(&db, |d| {
                    checkpoint(d, &turn.id, &format!("round:{}", round + 1), Some(&usage))
                })?;

                // The forced round had tools forbidden, so whatever it said is
                // the ending.
                if force_text {
                    return Ok(());
                }

                let tool_uses: Vec<(String, String, Value)> = result
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        LlmBlock::ToolUse { id, name, input } if name != STOP => {
                            Some((id.clone(), name.clone(), input.clone()))
                        }
                        _ => None,
                    })
                    .collect();

                if !tool_uses.is_empty() {
                    let mut tool_results: Vec<LlmContentBlock> = Vec::new();
                    for (id, name, input) in &tool_uses {
                        // Never start a tool once interrupted: stop before the
                        // side effect, not after it.
                        if cancel.is_cancelled() {
                            return Err(TurnFailure::Interrupted);
                        }
                        let on_log: Arc<dyn Fn(&str) + Send + Sync> = {
                            let bus = bus.clone();
                            let sid = session_id.clone();
                            let mid = message_id.clone();
                            let cid = id.clone();
                            Arc::new(move |line: &str| {
                                bus.publish(event(
                                    EventType::ToolLog,
                                    &sid,
                                    serde_json::to_value(ToolLogData {
                                        message_id: mid.clone(),
                                        call_id: cid.clone(),
                                        line: line.to_string(),
                                    })
                                    .unwrap_or_default(),
                                ));
                            })
                        };
                        let executed =
                            execute_tool(id, name, input, &program, &cancel, on_log).await;
                        append_part(
                            &db,
                            &bus,
                            &session_id,
                            &message_id,
                            &mut parts,
                            Part::ToolResult {
                                call_id: id.clone(),
                                output: Value::String(executed.output.clone()),
                                is_error: executed.is_error,
                                // The key only present when true.
                                interrupted: if executed.interrupted {
                                    Some(true)
                                } else {
                                    None
                                },
                            },
                        )?;
                        tool_results.push(LlmContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: executed.output,
                            is_error: executed.is_error,
                        });
                        with_db(&db, |d| {
                            checkpoint(d, &turn.id, &format!("tool:{name}"), Some(&usage))
                        })?;
                    }

                    // The report nudge rides INSIDE the tool_result message
                    // rather than arriving as a separate user turn: a model
                    // answers an inline nudge with text far more reliably than
                    // a standalone one, which tends to come back as empty
                    // thinking plus another stop.
                    if stop_requested && !said_something(&parts) {
                        if report_nudges < 1 {
                            report_nudges += 1;
                            tool_results.push(LlmContentBlock::Text {
                                text: REPORT_NUDGE.to_string(),
                            });
                            messages.push(LlmMessage {
                                role: LlmRole::User,
                                content: tool_results,
                            });
                            round += 1;
                            continue;
                        }
                        messages.push(LlmMessage {
                            role: LlmRole::User,
                            content: tool_results,
                        });
                        force_text = true;
                        round += 1;
                        continue;
                    }
                    messages.push(LlmMessage {
                        role: LlmRole::User,
                        content: tool_results,
                    });
                    if stop_requested {
                        return Ok(());
                    }
                    round += 1;
                    continue;
                }

                if stop_requested {
                    if said_something(&parts) {
                        return Ok(());
                    }
                    if report_nudges < 1 {
                        report_nudges += 1;
                        messages.push(LlmMessage {
                            role: LlmRole::User,
                            content: vec![LlmContentBlock::Text {
                                text: REPORT_NUDGE.to_string(),
                            }],
                        });
                        round += 1;
                        continue;
                    }
                    // The nudge came back mute — typically empty thinking plus
                    // another stop. Ending a prompt on a thinking-only
                    // assistant message is itself invalid, so drop that tail
                    // before forcing the text round.
                    if let Some(tail) = messages.last() {
                        if tail.role == LlmRole::Assistant
                            && tail
                                .content
                                .iter()
                                .all(|b| matches!(b, LlmContentBlock::Reasoning { .. }))
                        {
                            messages.pop();
                        }
                    }
                    force_text = true;
                    round += 1;
                    continue;
                }

                // Trailed off with no stop and no tools. Nudge — in memory
                // only, never persisted — with a cap, so a model that cannot
                // call `stop` still terminates.
                if nudges >= MAX_STOP_NUDGES {
                    return Ok(());
                }
                nudges += 1;
                messages.push(LlmMessage {
                    role: LlmRole::User,
                    content: vec![LlmContentBlock::Text {
                        text: STOP_NUDGE.to_string(),
                    }],
                });
                round += 1;
            }
        };
        match AssertUnwindSafe(fut).catch_unwind().await {
            Ok(r) => r,
            Err(panic) => Err(TurnFailure::Failed(BoughError::http(
                500,
                ErrorKind::Turn,
                format!("turn panicked: {}", panic_text(&panic)),
            ))),
        }
    };

    match loop_result {
        Ok(()) => {
            with_db(&db, |d| d.update_message(&message_id, &parts, false))?;
            index_quietly(&db, &message_id);
            with_db(&db, |d| {
                finish_turn(
                    d,
                    &turn.id,
                    FinalTurnStatus::Done,
                    FinishOpts {
                        usage: Some(usage.clone()),
                        step: Some("done".to_string()),
                        error: None,
                    },
                )
            })?;
            bus.publish(event(
                EventType::MessageFinished,
                &session_id,
                serde_json::to_value(MessageFinishedData {
                    message_id: message_id.clone(),
                })
                .unwrap_or_default(),
            ));
            bus.publish(event(
                EventType::TurnFinished,
                &session_id,
                turn_finished_data(&turn.id, &session_id, "done", None),
            ));
            // Delegation outcome for the tree view. Not an acceptance gate —
            // it records whether the TURN errored, nothing about whether the
            // work was any good.
            if delegated {
                with_db(&db, |d| d.set_session_outcome(&session_id, true))?;
            }
            // Hooks last, after the turn is durably finished: a `TurnEnd` hook
            // that injects a follow-up prompt must find an IDLE session, or
            // the wake rule queues work behind a turn that has already ended.
            apply_turn_hooks(
                &app,
                &session_id,
                &workspace,
                HookEvent::TurnEnd,
                serde_json::json!({ "ok": true }),
            );
            Ok(TurnOutcome {
                turn_id: turn.id,
                message_id,
                status: TurnOutcomeStatus::Done,
                error: None,
                usage,
            })
        }
        Err(failure) => {
            let (interrupted, raw) = match failure {
                TurnFailure::Interrupted => (true, None),
                TurnFailure::Failed(e) => (cancel.is_cancelled() || is_abort(&e), Some(e)),
            };
            let friendly = if interrupted {
                None
            } else {
                Some(friendly_turn_error(
                    &raw.as_ref().map(|e| e.to_string()).unwrap_or_default(),
                    &model,
                ))
            };
            let note = Part::Text {
                text: match &friendly {
                    None => stopped_note(&session_id, &deps),
                    Some(f) => format!("⚠︎ Turn failed: {f}"),
                },
            };
            parts.push(note.clone());
            with_db(&db, |d| d.update_message(&message_id, &parts, false))?;
            index_quietly(&db, &message_id);

            let status = if interrupted {
                TurnOutcomeStatus::Interrupted
            } else {
                TurnOutcomeStatus::Error
            };
            // The UI must never know more than the server log does — the raw
            // error is reported exactly once, and an interrupt is not an
            // error, so it is not reported at all.
            if !interrupted {
                if let Some(e) = &raw {
                    match &deps.report_error {
                        Some(f) => f(e, &session_id),
                        None => tracing::error!("turn failed [{session_id}]: {e}"),
                    }
                }
            }

            with_db(&db, |d| {
                finish_turn(
                    d,
                    &turn.id,
                    status.into(),
                    FinishOpts {
                        usage: Some(usage.clone()),
                        // `error ?? null`: the interrupt path CLEARS — "an
                        // interrupt is not an error".
                        error: friendly.clone(),
                        step: Some("ended".to_string()),
                    },
                )
            })?;
            bus.publish(event(
                EventType::MessagePart,
                &session_id,
                serde_json::to_value(MessagePartData {
                    message_id: message_id.clone(),
                    part: note,
                })
                .unwrap_or_default(),
            ));
            bus.publish(event(
                EventType::MessageFinished,
                &session_id,
                serde_json::to_value(MessageFinishedData {
                    message_id: message_id.clone(),
                })
                .unwrap_or_default(),
            ));
            bus.publish(event(
                EventType::TurnFinished,
                &session_id,
                turn_finished_data(&turn.id, &session_id, status.as_str(), friendly.as_deref()),
            ));
            if delegated {
                with_db(&db, |d| d.set_session_outcome(&session_id, false))?;
            }
            apply_turn_hooks(
                &app,
                &session_id,
                &workspace,
                HookEvent::TurnError,
                serde_json::json!({ "error": friendly.clone(), "interrupted": interrupted }),
            );
            Ok(TurnOutcome {
                turn_id: turn.id,
                message_id,
                status,
                error: friendly,
                usage,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// One round
// ---------------------------------------------------------------------------

/// One provider round, with the turn-level retry ring around it.
///
/// The ring is above whatever the client already does internally
/// (`with_retries`), and it exists for two failures that layer cannot fix. A
/// **truncated tool call** is the important one: the stream layer refuses to
/// invent `{}` for a call whose arguments were cut off, because executing it
/// would run the wrong program against the user's checkout. Re-streaming is
/// the only correct answer, and it is immediate — nothing is broken, a frame
/// was lost. The other is a **provider outage** long enough to outlive the
/// client's ~30s of backoff; a turn with all its work intact should not die
/// because the network flapped for a minute.
///
/// Every re-attempt emits `message.retry`, because a retried round re-streams
/// from the top and a client holding partial text must drop it.
#[allow(clippy::too_many_arguments)]
async fn run_round(
    llm: &Arc<dyn LlmClient>,
    params: LlmParams,
    on_text: OnText,
    cancel: &CancellationToken,
    bus: &Arc<Bus>,
    session_id: &str,
    message_id: &str,
    deps: &TurnDeps,
) -> Result<crate::types::LlmResult, TurnFailure> {
    let mut attempt: u32 = 1;
    loop {
        match llm
            .run(params.clone(), on_text.clone(), cancel.clone())
            .await
        {
            Ok(result) => return Ok(result),
            Err(err) => {
                let decision = classify_round_failure(
                    &err,
                    attempt,
                    &ClassifyOpts {
                        max_retries: deps.max_round_retries,
                        outage_delay_ms: deps.outage_delay_ms,
                    },
                );
                if !decision.retry || cancel.is_cancelled() {
                    return Err(TurnFailure::Failed(err));
                }
                bus.publish(event(
                    EventType::MessageRetry,
                    session_id,
                    serde_json::to_value(MessageRetryData {
                        message_id: message_id.to_string(),
                        attempt,
                        reason: decision.reason,
                    })
                    .unwrap_or_default(),
                ));
                abortable_delay(decision.delay_ms, Some(cancel))
                    .await
                    .map_err(|_| TurnFailure::Interrupted)?;
                attempt += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// A program's result as the model sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutedTool {
    pub output: String,
    pub is_error: bool,
    pub interrupted: bool,
}

/// What to say when the model calls a tool that does not exist.
///
/// The common case is not a hallucination, and answering it as one wasted
/// rounds. `view`, `bash`, `patch` and the rest are REAL — they are host
/// functions, called from inside the program — and a model under pressure
/// reaches for them at the tool layer, which is exactly where the names look
/// like they should live. A haiku run did this twice in one turn, got
/// "unknown tool: view", concluded the capability was missing, and rewrote
/// the whole approach around `bash`.
///
/// So when the name IS a host function, say where it lives and show the call.
/// The plain unknown-name case keeps the old message, which is right for a
/// name that really is invented.
fn unknown_tool_message(name: &str) -> String {
    let tools = format!("The only tools are `{RUN_STEPS}` and `{STOP}`.");
    if HostFnName::parse(name).is_none() {
        return format!("unknown tool: {name}. {tools}");
    }
    format!(
        "unknown tool: {name} — but `{name}` IS available: it is a host \
         function, already in scope inside the program you pass to `{RUN_STEPS}`, \
         not a tool of its own. {tools} Call it as code, e.g. \
         `const text = await {name}(...)`."
    )
}

/// Run one tool call. **This never fails.** Every failure — an unknown name, a
/// malformed input, a program that threw, a program the user stopped — is an
/// ordinary result the next round can act on, and a propagated one would end
/// the turn instead of letting the model recover.
async fn execute_tool(
    call_id: &str,
    name: &str,
    input: &Value,
    program: &ProgramRunner,
    cancel: &CancellationToken,
    on_log: Arc<dyn Fn(&str) + Send + Sync>,
) -> ExecutedTool {
    if name != RUN_STEPS {
        return ExecutedTool {
            output: unknown_tool_message(name),
            is_error: true,
            interrupted: false,
        };
    }

    let parsed: RunStepsInput = match serde_json::from_value(input.clone()) {
        Ok(p) => p,
        Err(e) => {
            return ExecutedTool {
                output: format!(
                    "invalid input for {RUN_STEPS}: {e}. It takes {{code: string, done?: boolean}}."
                ),
                is_error: true,
                interrupted: false,
            }
        }
    };

    let result = program(ProgramRun {
        code: parsed.code,
        call_id: call_id.to_string(),
        cancel: cancel.clone(),
        on_log,
    })
    .await;

    program_output(&result)
}

/// A program's result as the model sees it: the console output it printed,
/// plus the error that ended it when one did.
///
/// Partial output leads even on a failure. A program that printed twenty lines
/// and then threw has told the model most of what it needs; leading with the
/// error and dropping the lines would throw the round away.
pub fn program_output(result: &ProgramResult) -> ExecutedTool {
    let body = result.logs.join("\n");
    if result.ok {
        return ExecutedTool {
            output: if body.is_empty() {
                "(the program ran and printed nothing — console.log what you need to see)"
                    .to_string()
            } else {
                body
            },
            is_error: false,
            interrupted: false,
        };
    }
    let error = result
        .error
        .clone()
        .unwrap_or_else(|| "the program failed with no message".to_string());
    ExecutedTool {
        output: if body.is_empty() {
            error
        } else {
            format!("{body}\n\n{error}")
        },
        is_error: true,
        interrupted: result.interrupted.unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Has the model written a CLOSING summary — text after the last tool call?
///
/// Not "was there ever any text", which mid-turn narration satisfies, and
/// which produced the exact failure described in the module header: the more
/// an agent explained itself as it worked, the more reliably its turn ended on
/// a raw tool result.
fn said_something(parts: &[Part]) -> bool {
    let after = parts
        .iter()
        .rposition(|p| matches!(p, Part::ToolCall { .. }))
        .map(|i| i + 1)
        .unwrap_or(0);
    parts[after..]
        .iter()
        .any(|p| matches!(p, Part::Text { text } if !text.trim().is_empty()))
}

/// The usable prompt budget: the catalog window minus the reservation every
/// round makes for output. `None` when the model is not in the catalog — an
/// unknown window must not become a fabricated limit that fails turns that
/// would have worked.
pub fn usable_context_limit(model: &str, max_tokens: i64) -> Option<i64> {
    context_window_for(model).map(|w| w - max_tokens)
}

/// Sum a round into the turn's running total.
fn fold_usage(total: &mut Usage, round: &Usage) {
    total.input_tokens += round.input_tokens;
    total.output_tokens += round.output_tokens;
    total.reasoning_tokens =
        Some(total.reasoning_tokens.unwrap_or(0) + round.reasoning_tokens.unwrap_or(0));
    total.cache_read_tokens =
        Some(total.cache_read_tokens.unwrap_or(0) + round.cache_read_tokens.unwrap_or(0));
    total.cache_write_tokens =
        Some(total.cache_write_tokens.unwrap_or(0) + round.cache_write_tokens.unwrap_or(0));
    total.cost_usd = Some(total.cost_usd.unwrap_or(0.0) + round.cost_usd.unwrap_or(0.0));
}

/// The interrupt note, naming the background shells that survive it.
///
/// They are detached on purpose — a `bashBg` or an auto-backgrounded build
/// outlives the turn — so a stop that silently leaves them running looks like
/// a stop that did not work. Absent seam = say nothing, rather than claim
/// there were none.
fn stopped_note(session_id: &str, deps: &TurnDeps) -> String {
    let survivors = deps
        .surviving_jobs
        .as_ref()
        .map(|f| f(session_id))
        .unwrap_or_default();
    if survivors.is_empty() {
        return STOPPED_NOTE.to_string();
    }
    format!(
        "{STOPPED_NOTE}\n{} still running — {} the interrupt.",
        survivors.join(", "),
        if survivors.len() == 1 {
            "it survives"
        } else {
            "they survive"
        }
    )
}

/// Keyword search is maintained on insert. Failing to index is a degraded
/// search, not a failed turn, so it never propagates.
fn index_quietly(db: &SharedDb, message_id: &str) {
    let indexed = with_db(db, |d| -> Result<(), BoughError> {
        if let Some(stored) = d.get_message(message_id)? {
            d.index_message(&stored)?;
        }
        Ok(())
    });
    if let Err(err) = indexed {
        tracing::error!("failed to index message {message_id}: {err}");
    }
}

/// Provider failures in plain language, with the fix at hand.
///
/// Error text is a product surface: what a failure says determines whether the
/// user fixes it or files a bug. A missing key must not read as a model
/// outage, and a provider's multi-line escaped-JSON 400 body must not be
/// pasted into a transcript card — it is folded to one line, because these
/// also travel upward as a subagent's report.
///
/// THE ENV VAR, BECAUSE THERE IS NO KEYS PANEL: keys are environment variables
/// in this tree, and a message naming a surface that does not exist is the
/// same defect as a legend naming a key that is not bound.
pub fn friendly_turn_error(msg: &str, model: &str) -> String {
    static NO_CF_ACCOUNT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"CLOUDFLARE_ACCOUNT_ID is not set").unwrap());
    static MISSING_KEY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)Could not resolve authentication method|apiKey or authToken|API_KEY is not set",
        )
        .unwrap()
    });
    static REJECTED_KEY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)invalid x-api-key|authentication_error|Incorrect API key").unwrap()
    });
    static HTTP_TAIL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s):\s*(\d{3})\s+(.+)$").unwrap());
    static TOOL_FORMAT_400: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)tool_calls|tool_call_id|must be followed by tool").unwrap()
    });

    let key = provider_for(model);
    let provider = match key {
        Provider::Openai => "OpenAI",
        Provider::Openrouter => "OpenRouter",
        Provider::Cloudflare => "Cloudflare",
        Provider::Cerebras => "Cerebras",
        Provider::Anthropic => "Anthropic",
    };
    let env_var = api_key_env(key);

    // Cloudflare is account-scoped: a valid key with no account id still
    // cannot reach an endpoint, and the generic "no key" line would send the
    // reader to the wrong var.
    if NO_CF_ACCOUNT.is_match(msg) {
        return "No Cloudflare account id set — export CLOUDFLARE_ACCOUNT_ID and restart the \
                bough server."
            .to_string();
    }

    if MISSING_KEY.is_match(msg) {
        return format!(
            "No {provider} API key set — export {env_var} and restart the bough server."
        );
    }
    if REJECTED_KEY.is_match(msg) {
        return format!(
            "{provider} rejected the key in {env_var} — fix it and restart the bough server."
        );
    }
    if let Some(caps) = HTTP_TAIL.captures(msg) {
        let status: u16 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let body = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        if TOOL_FORMAT_400.is_match(body) {
            return format!(
                "{provider} rejected the tool-call formatting ({status}); a repaired retry \
                 usually clears it."
            );
        }
        if status >= 400 {
            return format!(
                "{provider} error {status}: {}",
                short_reason_text(body, 120)
            );
        }
    }
    msg.to_string()
}

/// Thousands separators (the TS `toLocaleString()`), for the overflow message.
fn locale(n: i64) -> String {
    let raw = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && (raw.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn parse_effort(s: &str) -> Option<Effort> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

/// Run one synchronous db operation under the shared lock. Never held across
/// an await; a poisoned lock (a panicking test elsewhere) is recovered rather
/// than cascaded.
fn with_db<R>(db: &SharedDb, f: impl FnOnce(&dyn Db) -> R) -> R {
    let guard = db.lock().unwrap_or_else(|p| p.into_inner());
    f(&*guard)
}

fn event(r#type: EventType, session_id: &str, data: Value) -> EventInput {
    EventInput {
        r#type,
        session_id: Some(session_id.to_string()),
        data,
    }
}

/// `turn.finished` data — the `error` key omitted entirely when absent, not
/// null (`deepEqual` in the tests pins the exact shape).
fn turn_finished_data(turn_id: &str, session_id: &str, status: &str, error: Option<&str>) -> Value {
    let mut data = serde_json::Map::new();
    data.insert("turnId".to_string(), json!(turn_id));
    data.insert("sessionId".to_string(), json!(session_id));
    data.insert("status".to_string(), json!(status));
    if let Some(error) = error {
        data.insert("error".to_string(), json!(error));
    }
    Value::Object(data)
}

/// Persist one part and announce it. The parts array is the message's whole
/// content; the db write is a wholesale overwrite with `pending: true`.
fn append_part(
    db: &SharedDb,
    bus: &Arc<Bus>,
    session_id: &str,
    message_id: &str,
    parts: &mut Vec<Part>,
    part: Part,
) -> Result<(), BoughError> {
    parts.push(part.clone());
    with_db(db, |d| d.update_message(message_id, parts, true))?;
    bus.publish(event(
        EventType::MessagePart,
        session_id,
        serde_json::to_value(MessagePartData {
            message_id: message_id.to_string(),
            part,
        })
        .unwrap_or_default(),
    ));
    Ok(())
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic".to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests — ports of `src/turn/runner.test.ts` plus the runner-driven acceptance
// tests from `src/turn/queue.test.ts` (interrupt mid-program, two rapid
// messages, the truncated tool call). Everything offline: a scripted fake
// LlmClient, a fake program runner, an in-memory database, no worker and no
// socket.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::events::BoughEvent;
    use crate::schema::parts::TurnStatus;
    use crate::turn::testkit::{reasoning, run_steps, scripted_llm, stop, text, ScriptedRound};

    use std::sync::Mutex;

    // ---- fixtures ----------------------------------------------------------

    /// The code and call id of one executed program — what the TS fixture
    /// keeps of each `ProgramRun`.
    #[derive(Clone, Debug, PartialEq)]
    struct ProgramCall {
        code: String,
        call_id: String,
    }

    struct Fixture {
        db: SharedDb,
        ctx: AppCtx,
        events: Arc<Mutex<Vec<BoughEvent>>>,
        session: Session,
        registry: Arc<TurnRegistry>,
        programs: Arc<Mutex<Vec<ProgramCall>>>,
        reported: Arc<Mutex<Vec<BoughError>>>,
        deps: TurnDeps,
    }

    type ProgramBehavior =
        Arc<dyn Fn(ProgramRun) -> BoxFuture<'static, ProgramResult> + Send + Sync>;

    fn ok_result() -> ProgramResult {
        ProgramResult {
            ok: true,
            logs: vec![],
            error: None,
            interrupted: None,
        }
    }

    fn logs_result(lines: &[&str]) -> ProgramResult {
        ProgramResult {
            ok: true,
            logs: lines.iter().map(|s| s.to_string()).collect(),
            error: None,
            interrupted: None,
        }
    }

    struct FixtureOpts {
        llm: Arc<dyn LlmClient>,
        program: Option<ProgramBehavior>,
        kind: SessionKind,
        model: Option<String>,
    }

    fn opts(llm: Arc<dyn LlmClient>) -> FixtureOpts {
        FixtureOpts {
            llm,
            program: None,
            kind: SessionKind::Root,
            model: Some("claude-opus-4-8".into()),
        }
    }

    fn fixture(o: FixtureOpts) -> Fixture {
        use crate::db::sqlite_db::{DbOptions, SqliteDb};

        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let bus = Arc::new(Bus::new(crate::types::system_clock()));
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(vec![]));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e| sink.lock().unwrap().push(e.clone())));

        let session = with_db(&db, |d| {
            d.create_session(Session {
                id: Uuid::new_v4().to_string(),
                title: "test session".into(),
                kind: o.kind,
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

        let registry = Arc::new(TurnRegistry::new());
        let programs: Arc<Mutex<Vec<ProgramCall>>> = Arc::new(Mutex::new(vec![]));
        let reported: Arc<Mutex<Vec<BoughError>>> = Arc::new(Mutex::new(vec![]));

        let behavior: ProgramBehavior = o
            .program
            .unwrap_or_else(|| Arc::new(|_run| async { ok_result() }.boxed()));
        let recorded = programs.clone();
        let program: ProgramRunner = Arc::new(move |run: ProgramRun| {
            recorded.lock().unwrap().push(ProgramCall {
                code: run.code.clone(),
                call_id: run.call_id.clone(),
            });
            behavior(run)
        });

        let r = reported.clone();
        let deps = TurnDeps {
            registry: Some(registry.clone()),
            // Collected rather than logged: an intentional failure should not
            // print a stack, and the reporting itself is worth asserting.
            report_error: Some(Arc::new(move |err, _sid| {
                r.lock().unwrap().push(err.clone())
            })),
            // A stub prompt: what assembly produces is the prompt suite's
            // subject, and reading twenty markdown files here would only
            // couple this test to their text.
            assemble: Some(Arc::new(|_input| AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            })),
            program: Some(program),
            outage_delay_ms: Some(0),
            ..Default::default()
        };

        let ctx = AppCtx {
            db: db.clone(),
            bus,
            llm: Some(o.llm),
            model: o.model,
            effort: None,
            now: crate::types::system_clock(),
            cheap: None,
            host: Arc::new(crate::types::HostState::new()),
            starter: Arc::new(std::sync::RwLock::new(None)),
            turn_registry: registry.clone(),
            model_defaults_path: None,
        };

        Fixture {
            db,
            ctx,
            events,
            session,
            registry,
            programs,
            reported,
            deps,
        }
    }

    fn user_message(db: &SharedDb, session_id: &str, body: &str, at: i64) -> Message {
        with_db(db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                role: Role::User,
                parts: vec![Part::Text {
                    text: body.to_string(),
                }],
                pending: false,
                created_at: at,
            })
        })
        .unwrap()
    }

    fn now_ms() -> i64 {
        (crate::types::system_clock())()
    }

    /// Every content block of every message in a payload, flattened.
    fn all_blocks(messages: &[LlmMessage]) -> Vec<&LlmContentBlock> {
        messages.iter().flat_map(|m| m.content.iter()).collect()
    }

    fn parts_of(db: &SharedDb, message_id: &str) -> Vec<Part> {
        with_db(db, |d| d.get_message(message_id))
            .unwrap()
            .unwrap()
            .parts
    }

    fn part_types(parts: &[Part]) -> Vec<&'static str> {
        parts
            .iter()
            .map(|p| match p {
                Part::Text { .. } => "text",
                Part::Reasoning { .. } => "reasoning",
                Part::ToolCall { .. } => "tool_call",
                Part::ToolResult { .. } => "tool_result",
                Part::Image { .. } => "image",
                Part::Ask { .. } => "ask",
                Part::Workflow { .. } => "workflow",
            })
            .collect()
    }

    fn event_types(events: &[BoughEvent]) -> Vec<&'static str> {
        events.iter().map(|e| e.r#type.as_str()).collect()
    }

    fn messages_json(params: &LlmParams) -> String {
        serde_json::to_string(&params.messages).unwrap()
    }

    async fn finish(started: StartedTurn) -> TurnOutcome {
        started.done.await.unwrap().unwrap()
    }

    // ---- the multi-round turn ----------------------------------------------

    #[tokio::test]
    async fn a_multi_round_turn_runs_the_program_ends_on_stop_and_replays_only_signed_reasoning() {
        let llm = scripted_llm(vec![
            // Round 1: thinks, narrates, runs a program.
            ScriptedRound {
                content: vec![
                    reasoning(
                        "weighing two approaches",
                        Some(json!({"signature": "sig-1"})),
                    ),
                    text("Looking at the file now."),
                    run_steps("call-1", "console.log(await bash('ls'))"),
                ],
                deltas: vec!["Looking at ".into(), "the file now.".into()],
                usage: Some(Usage {
                    input_tokens: 1_000,
                    output_tokens: 40,
                    cache_read_tokens: Some(200),
                    ..Default::default()
                }),
                throws: None,
            },
            // Round 2: reports and stops, in the same response.
            ScriptedRound {
                content: vec![text("Listed the directory: three files."), stop("stop-1")],
                usage: Some(Usage {
                    input_tokens: 1_200,
                    output_tokens: 20,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]);
        let mut o = opts(llm.clone());
        o.program = Some(Arc::new(|_| {
            async { logs_result(&["a.ts", "b.ts", "c.ts"]) }.boxed()
        }));
        let f = fixture(o);

        // A previous turn's transcript, including a reasoning part. This is
        // what must never come back.
        with_db(&f.db, |d| {
            d.create_message(Message {
                id: Uuid::new_v4().to_string(),
                session_id: f.session.id.clone(),
                role: Role::Supervisor,
                parts: vec![
                    Part::Reasoning {
                        text: "PRIOR-THINKING-DO-NOT-REPLAY".into(),
                        meta: None,
                        model: None,
                    },
                    Part::Text {
                        text: "Earlier answer.".into(),
                    },
                    Part::ToolCall {
                        id: "old-1".into(),
                        name: RUN_STEPS.into(),
                        input: json!({"code": "1"}),
                    },
                    Part::ToolResult {
                        call_id: "old-1".into(),
                        output: json!("1"),
                        is_error: false,
                        interrupted: None,
                    },
                ],
                pending: false,
                created_at: 1_500,
            })
        })
        .unwrap();
        user_message(&f.db, &f.session.id, "list the files", 2_000);

        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        let outcome = finish(started).await;

        // ── the loop ran to a clean end ──
        assert_eq!(outcome.status, TurnOutcomeStatus::Done);
        let calls = llm.calls();
        assert_eq!(calls.len(), 2, "two rounds");
        {
            let programs = f.programs.lock().unwrap();
            assert_eq!(programs.len(), 1, "one program");
            assert_eq!(programs[0].code, "console.log(await bash('ls'))");
            assert_eq!(programs[0].call_id, "call-1");
        }

        // ── the transcript ──
        let parts = parts_of(&f.db, &message.id);
        assert_eq!(
            part_types(&parts),
            vec!["reasoning", "text", "tool_call", "tool_result", "text"]
        );
        match &parts[3] {
            Part::ToolResult {
                output,
                is_error,
                interrupted,
                ..
            } => {
                assert_eq!(output, &json!("a.ts\nb.ts\nc.ts"));
                assert!(!is_error);
                assert_eq!(*interrupted, None);
            }
            other => panic!("expected tool_result, got {other:?}"),
        }
        assert!(
            !with_db(&f.db, |d| d.get_message(&message.id))
                .unwrap()
                .unwrap()
                .pending,
            "the message is closed"
        );
        // `stop` is loop control: it is never persisted, so it can never replay.
        assert!(!parts
            .iter()
            .any(|p| matches!(p, Part::ToolCall { name, .. } if name == STOP)));

        // ── round 1's payload: the prior turn's reasoning is gone ──
        let round1 = all_blocks(&calls[0].messages);
        assert_eq!(
            round1
                .iter()
                .filter(|b| matches!(b, LlmContentBlock::Reasoning { .. }))
                .count(),
            0,
            "a persisted reasoning part must not reach the provider"
        );
        assert!(
            !messages_json(&calls[0]).contains("PRIOR-THINKING-DO-NOT-REPLAY"),
            "not as a reasoning block and not smuggled in as text either"
        );
        // The rest of the prior turn DID replay — the drop is surgical, not a
        // discard.
        assert!(round1
            .iter()
            .any(|b| matches!(b, LlmContentBlock::Text { text } if text == "Earlier answer.")));
        assert!(round1
            .iter()
            .any(|b| matches!(b, LlmContentBlock::ToolUse { id, .. } if id == "old-1")));
        assert!(round1
            .iter()
            .any(|b| matches!(b, LlmContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "old-1")));
        assert!(round1
            .iter()
            .any(|b| matches!(b, LlmContentBlock::Text { text } if text == "list the files")));

        // ── round 2's payload: the CURRENT turn's reasoning IS echoed, with
        // its meta ── Different rule, same file: a provider that signs
        // thinking rejects a tool call whose thinking was altered, so within a
        // turn the block travels back verbatim.
        let echoed: Vec<&LlmContentBlock> = all_blocks(&calls[1].messages)
            .into_iter()
            .filter(|b| matches!(b, LlmContentBlock::Reasoning { .. }))
            .collect();
        assert_eq!(echoed.len(), 1);
        match echoed[0] {
            LlmContentBlock::Reasoning { meta, .. } => {
                assert_eq!(meta, &Some(json!({"signature": "sig-1"})))
            }
            _ => unreachable!(),
        }
        // ...and the pending message itself is not in its own history.
        assert!(!messages_json(&calls[0]).contains(&message.id));

        // ── the tools the model saw ──
        assert_eq!(
            calls[0]
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec![RUN_STEPS, STOP]
        );
        assert_eq!(
            calls[0].tools, *TOOLS,
            "byte-stable across rounds and sessions"
        );

        // ── usage ──
        let turn = with_db(&f.db, |d| d.turn_for_message(&message.id))
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, TurnStatus::Done);
        assert_eq!(turn.usage.as_ref().unwrap().input_tokens, 2_200);
        assert_eq!(turn.usage.as_ref().unwrap().output_tokens, 60);
        let session_usage = with_db(&f.db, |d| d.session_usage(&f.session.id)).unwrap();
        assert_eq!(session_usage.input_tokens, 2_200);
        assert_eq!(session_usage.cache_read_tokens, 200);

        // ── events ──
        {
            let events = f.events.lock().unwrap();
            let types = event_types(&events);
            assert!(types.contains(&"message.started"));
            assert!(types.contains(&"message.delta"));
            assert!(types.contains(&"message.part"));
            assert_eq!(
                types.iter().filter(|t| **t == "message.finished").count(),
                1
            );
            let finished = events
                .iter()
                .find(|e| e.r#type == EventType::TurnFinished)
                .unwrap();
            assert_eq!(
                finished.data,
                json!({ "turnId": turn.id, "sessionId": f.session.id, "status": "done" })
            );
        }

        // ── THE ACCEPTANCE CRITERION, against a transcript the runner wrote ──
        // A second turn over the same session, on the SAME model: the
        // reasoning this turn persisted replays, and it replays VERBATIM —
        // the payload that made it valid has to survive the round trip through
        // the database untouched.
        let llm2 = scripted_llm(vec![ScriptedRound {
            content: vec![text("Nothing further."), stop("stop-2")],
            ..Default::default()
        }]);
        let mut ctx2 = f.ctx.clone();
        ctx2.llm = Some(llm2.clone());
        user_message(&f.db, &f.session.id, "anything else?", 5_000);
        finish(begin_turn(&ctx2, &f.session.id, f.deps.clone()).unwrap()).await;

        let calls2 = llm2.calls();
        let replayed = all_blocks(&calls2[0].messages);
        let replayed_thinking: Vec<&&LlmContentBlock> = replayed
            .iter()
            .filter(|b| matches!(b, LlmContentBlock::Reasoning { .. }))
            .collect();
        assert_eq!(
            replayed_thinking.len(),
            1,
            "the signed block from the previous turn replays"
        );
        match **replayed_thinking[0] {
            LlmContentBlock::Reasoning { ref meta, .. } => assert_eq!(
                meta,
                &Some(json!({"signature": "sig-1"})),
                "verbatim: a provider rejects a thinking block whose content was altered"
            ),
            _ => unreachable!(),
        }
        assert!(
            !messages_json(&calls2[0]).contains("PRIOR-THINKING-DO-NOT-REPLAY"),
            "an UNSIGNED part still never replays — there is nothing to vouch for it"
        );
        // The turn's own words and its program's result did replay.
        assert!(replayed.iter().any(
            |b| matches!(b, LlmContentBlock::Text { text } if text == "Listed the directory: three files.")
        ));
        assert!(replayed
            .iter()
            .any(|b| matches!(b, LlmContentBlock::ToolResult { content, .. } if content == "a.ts\nb.ts\nc.ts")));

        // ── the gate is the model, and it is the only gate ──
        // Same transcript, different model: the signature is not valid for it,
        // so the block is dropped rather than sent to be discarded (or
        // rejected) downstream.
        let llm3 = scripted_llm(vec![ScriptedRound {
            content: vec![text("Still nothing."), stop("stop-3")],
            ..Default::default()
        }]);
        let mut ctx3 = f.ctx.clone();
        ctx3.llm = Some(llm3.clone());
        ctx3.model = Some("a-different-model".into());
        user_message(&f.db, &f.session.id, "and now?", 6_000);
        finish(begin_turn(&ctx3, &f.session.id, f.deps.clone()).unwrap()).await;

        let calls3 = llm3.calls();
        assert_eq!(
            all_blocks(&calls3[0].messages)
                .iter()
                .filter(|b| matches!(b, LlmContentBlock::Reasoning { .. }))
                .count(),
            0,
            "reasoning signed by another model does not replay"
        );
        assert!(
            !messages_json(&calls3[0]).contains("weighing two approaches"),
            "and its text is not smuggled across as prose either"
        );
    }

    // ---- ending rules ------------------------------------------------------

    #[tokio::test]
    async fn a_turn_that_would_end_mute_is_nudged_for_a_closing_report() {
        let llm = scripted_llm(vec![
            // Runs a program and tries to stop having said nothing.
            ScriptedRound {
                content: vec![run_steps("c1", "console.log(1)"), stop("stop-1")],
                ..Default::default()
            },
            // Answers the nudge with the report, and stops again.
            ScriptedRound {
                content: vec![text("Done: printed 1."), stop("stop-2")],
                ..Default::default()
            },
        ]);
        let mut o = opts(llm.clone());
        o.program = Some(Arc::new(|_| async { logs_result(&["1"]) }.boxed()));
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "print one", 2_000);

        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        assert_eq!(finish(started).await.status, TurnOutcomeStatus::Done);

        let calls = llm.calls();
        assert_eq!(
            calls.len(),
            2,
            "the stop was not honored while the turn was mute"
        );
        // The nudge rides inside the tool_result message, never as a separate
        // turn.
        let second = calls[1].messages.last().unwrap();
        assert_eq!(second.role, LlmRole::User);
        assert_eq!(
            second
                .content
                .iter()
                .filter(|b| matches!(b, LlmContentBlock::ToolResult { .. }))
                .count(),
            1
        );
        assert!(second
            .content
            .iter()
            .any(|b| matches!(b, LlmContentBlock::Text { text } if text.contains("[harness]"))));

        // The nudge is loop control: it is never persisted.
        let parts = parts_of(&f.db, &message.id);
        assert!(!serde_json::to_string(&parts).unwrap().contains("[harness]"));
        assert!(matches!(parts.last(), Some(Part::Text { .. })));
    }

    #[tokio::test]
    async fn a_persistently_mute_turn_is_forced_into_a_text_only_round() {
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![run_steps("c1", "console.log(1)"), stop("stop-1")],
                ..Default::default()
            },
            // Answers the nudge with empty thinking and another stop — the
            // observed failure.
            ScriptedRound {
                content: vec![reasoning("", None), stop("stop-2")],
                ..Default::default()
            },
            // The forced round has tools forbidden, so it can only speak.
            ScriptedRound {
                content: vec![text("I printed 1.")],
                ..Default::default()
            },
        ]);
        let mut o = opts(llm.clone());
        o.program = Some(Arc::new(|_| async { logs_result(&["1"]) }.boxed()));
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "print one", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        finish(started).await;

        let calls = llm.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[2].tool_choice_none, "the last resort forbids tools");
        let parts = parts_of(&f.db, &message.id);
        assert_eq!(
            parts.last(),
            Some(&Part::Text {
                text: "I printed 1.".into()
            })
        );
    }

    #[tokio::test]
    async fn a_turn_that_trails_off_without_stop_is_nudged_and_the_nudges_are_bounded() {
        // Never calls stop, never calls a tool: the runaway shape.
        let rounds: Vec<ScriptedRound> = (0..8)
            .map(|_| ScriptedRound {
                content: vec![text("...thinking out loud")],
                ..Default::default()
            })
            .collect();
        let llm = scripted_llm(rounds);
        let f = fixture(opts(llm.clone()));
        user_message(&f.db, &f.session.id, "hello", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        let outcome = finish(started).await;

        assert_eq!(
            outcome.status,
            TurnOutcomeStatus::Done,
            "a nudge cap ends the turn, it does not fail it"
        );
        let calls = llm.calls();
        assert_eq!(calls.len() as u32, MAX_STOP_NUDGES + 1);
        // Every nudge lived in memory only.
        assert!(!serde_json::to_string(&parts_of(&f.db, &message.id))
            .unwrap()
            .contains("[harness]"));
        assert!(calls[1].messages.iter().any(|m| m.role == LlmRole::User
            && m.content.iter().any(
                |b| matches!(b, LlmContentBlock::Text { text } if text.contains("still open"))
            )));
    }

    #[tokio::test]
    async fn an_emitted_stop_sentinel_ends_the_turn_and_is_stripped_from_the_transcript() {
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![text("All done.\n<stop/>")],
            ..Default::default()
        }]);
        let f = fixture(opts(llm.clone()));
        user_message(&f.db, &f.session.id, "go", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        finish(started).await;

        assert_eq!(
            llm.calls().len(),
            1,
            "the sentinel is honored as a stop, not nudged"
        );
        let parts = parts_of(&f.db, &message.id);
        assert_eq!(
            parts[0],
            Part::Text {
                text: "All done.".into()
            }
        );
    }

    // ---- failure paths -----------------------------------------------------

    #[tokio::test]
    async fn a_failing_program_is_a_tool_result_the_next_round_can_act_on_not_a_turn_error() {
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![run_steps("c1", "boom()")],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("That threw; I will try another way."), stop("stop-1")],
                ..Default::default()
            },
        ]);
        let mut o = opts(llm);
        o.program = Some(Arc::new(|_| {
            async {
                ProgramResult {
                    ok: false,
                    logs: vec!["about to fail".into()],
                    error: Some("ReferenceError: boom is not defined".into()),
                    interrupted: None,
                }
            }
            .boxed()
        }));
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "run it", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        assert_eq!(finish(started).await.status, TurnOutcomeStatus::Done);

        let parts = parts_of(&f.db, &message.id);
        match parts
            .iter()
            .find(|p| matches!(p, Part::ToolResult { .. }))
            .unwrap()
        {
            Part::ToolResult {
                output, is_error, ..
            } => {
                assert!(is_error);
                // Partial output leads: the lines it printed are most of what
                // the model needs.
                let out = output.as_str().unwrap();
                assert!(out.starts_with("about to fail\n\nReferenceError"), "{out}");
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn a_malformed_run_steps_input_is_refused_rather_than_executed() {
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![LlmBlock::ToolUse {
                    id: "c1".into(),
                    name: RUN_STEPS.into(),
                    input: json!({"code": 42}),
                }],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("Retrying with a string."), stop("stop-1")],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm));
        user_message(&f.db, &f.session.id, "go", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        finish(started).await;

        assert_eq!(f.programs.lock().unwrap().len(), 0, "nothing was executed");
        let parts = parts_of(&f.db, &message.id);
        match parts
            .iter()
            .find(|p| matches!(p, Part::ToolResult { .. }))
            .unwrap()
        {
            Part::ToolResult {
                output, is_error, ..
            } => {
                assert!(is_error);
                assert!(output
                    .as_str()
                    .unwrap()
                    .contains("invalid input for run_steps"));
                assert!(output
                    .as_str()
                    .unwrap()
                    .contains("It takes {code: string, done?: boolean}."));
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn an_unknown_tool_name_is_answered_not_executed() {
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![LlmBlock::ToolUse {
                    id: "c1".into(),
                    name: "read_file".into(),
                    input: json!({}),
                }],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("Right, I only have run_steps."), stop("stop-1")],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm));
        user_message(&f.db, &f.session.id, "go", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        finish(started).await;
        assert_eq!(f.programs.lock().unwrap().len(), 0);
        let parts = parts_of(&f.db, &message.id);
        match parts
            .iter()
            .find(|p| matches!(p, Part::ToolResult { .. }))
            .unwrap()
        {
            Part::ToolResult { output, .. } => {
                assert!(output.as_str().unwrap().contains("unknown tool: read_file"));
            }
            _ => unreachable!(),
        }
    }

    /// `view` is not invented — it is the file-reading host function. A model
    /// that reaches for it at the TOOL layer has the right capability and the
    /// wrong place, and answering that with a bare "unknown tool" reads as
    /// "bough cannot read files". A haiku run drew exactly that conclusion and
    /// rebuilt its approach around `bash`, twice, in one turn.
    #[tokio::test]
    async fn a_host_function_called_as_a_tool_is_told_where_it_actually_lives() {
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![LlmBlock::ToolUse {
                    id: "c1".into(),
                    name: "view".into(),
                    input: json!({"path": "./x.py"}),
                }],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![
                    text("Right, view is a function inside the program."),
                    stop("stop-1"),
                ],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm));
        user_message(&f.db, &f.session.id, "go", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        finish(started).await;
        assert_eq!(f.programs.lock().unwrap().len(), 0);
        let parts = parts_of(&f.db, &message.id);
        let out = match parts
            .iter()
            .find(|p| matches!(p, Part::ToolResult { .. }))
            .unwrap()
        {
            Part::ToolResult { output, .. } => output.as_str().unwrap().to_string(),
            _ => unreachable!(),
        };
        // It must say the capability EXISTS, where it lives, and how to call
        // it — "the only tools are ..." alone leaves the model to guess the
        // recovery.
        assert!(out.contains("`view` IS available"), "{out}");
        assert!(out.contains("host function"), "{out}");
        assert!(out.contains("await view("), "{out}");
    }

    #[tokio::test]
    async fn a_provider_failure_ends_the_turn_with_a_message_a_closed_row_and_a_closed_message() {
        let llm = scripted_llm(vec![ScriptedRound {
            throws: Some(BoughError::llm_with("Anthropic: 400 bad prompt", 400, None)),
            ..Default::default()
        }]);
        let f = fixture(opts(llm));
        user_message(&f.db, &f.session.id, "go", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        let outcome = finish(started).await;

        assert_eq!(outcome.status, TurnOutcomeStatus::Error);
        let stored = with_db(&f.db, |d| d.get_message(&message.id))
            .unwrap()
            .unwrap();
        assert!(!stored.pending, "a failed turn still closes its message");
        match stored.parts.last().unwrap() {
            Part::Text { text } => assert!(text.contains("⚠︎ Turn failed"), "{text}"),
            other => panic!("expected text, got {other:?}"),
        }
        let turn = with_db(&f.db, |d| d.turn_for_message(&message.id))
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, TurnStatus::Error);
        assert!(turn.error.is_some());
        assert_eq!(
            f.reported.lock().unwrap().len(),
            1,
            "the raw error reached the server log, once"
        );
        assert!(
            !with_db(&f.db, |d| d.busy_session_ids())
                .unwrap()
                .contains(&f.session.id),
            "the session is free again"
        );
    }

    #[tokio::test]
    async fn a_turn_that_would_overflow_the_context_window_fails_naming_the_limit() {
        let llm = scripted_llm(vec![
            // One enormous round, then the loop should refuse to send another.
            ScriptedRound {
                content: vec![run_steps("c1", "console.log(1)")],
                usage: Some(Usage {
                    input_tokens: 100_000_000,
                    output_tokens: 10,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]);
        let mut o = opts(llm.clone());
        o.program = Some(Arc::new(|_| async { logs_result(&["1"]) }.boxed()));
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "go", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        let outcome = finish(started).await;

        assert_eq!(outcome.status, TurnOutcomeStatus::Error);
        assert_eq!(llm.calls().len(), 1, "the doomed round was never sent");
        let parts = parts_of(&f.db, &message.id);
        let note = match parts.last().unwrap() {
            Part::Text { text } => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        assert!(note.contains("context window"), "{note}");
        assert!(note.contains("claude-opus-4-8"), "{note}");
        assert!(
            note.contains("Compact or fork"),
            "it names the move that resolves it: {note}"
        );
    }

    // ---- the server seam ---------------------------------------------------

    #[tokio::test]
    async fn create_turn_starter_runs_a_turn_when_idle_and_only_queues_when_busy() {
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![text("first answer"), stop("s1")],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("second answer"), stop("s2")],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm.clone()));
        let start = create_turn_starter(f.deps.clone());

        let first = user_message(&f.db, &f.session.id, "one", now_ms());
        start.start_turn(&f.ctx, &f.session, &first);
        // Claim + create are inline, so the session is claimed by now.
        assert!(f.registry.is_running(&f.session.id));

        // A second call while busy must not start a second turn on the same
        // session. (The TS test watched `llm.calls`; here the loop is spawned,
        // so the synchronous evidence is the turn table — begin's setup runs
        // inline, and no second row means no second turn began.)
        let second = user_message(&f.db, &f.session.id, "two", now_ms());
        start.start_turn(&f.ctx, &f.session, &second);
        assert_eq!(
            with_db(&f.db, |d| d.turns_for_session(&f.session.id))
                .unwrap()
                .len(),
            1,
            "the busy session started nothing"
        );

        // The queued message drains into a fresh turn of its own.
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if with_db(&f.db, |d| d.busy_session_ids()).unwrap().is_empty()
                && llm.calls().len() == 2
            {
                break;
            }
        }
        assert_eq!(llm.calls().len(), 2);
        assert_eq!(
            with_db(&f.db, |d| d.turns_for_session(&f.session.id))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(with_db(&f.db, |d| d.busy_session_ids()).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_subagent_session_records_its_turns_outcome_for_the_tree_view() {
        let ok = scripted_llm(vec![ScriptedRound {
            content: vec![text("Report: did the thing."), stop("stop-1")],
            ..Default::default()
        }]);
        let mut o = opts(ok);
        o.kind = SessionKind::Subagent;
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "do the thing", 2_000);
        finish(begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap()).await;
        assert_eq!(
            with_db(&f.db, |d| d.get_session(&f.session.id))
                .unwrap()
                .unwrap()
                .outcome_ok,
            Some(true)
        );
    }

    #[tokio::test]
    async fn a_sessions_model_pin_beats_the_global_default_the_way_effort_does() {
        // `model` and `effort` are per-session OVERRIDES; absent = the global
        // default. `AppCtx.model` IS that global default, so reading it first
        // would make `set_session_model` a no-op on any install that sets
        // `BOUGH_MODEL` — and the two fields would disagree about their own
        // rule, since `effort` already resolves session-first.
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![text("done"), stop("stop-1")],
            ..Default::default()
        }]);
        let f = fixture(opts(llm.clone()));
        with_db(&f.db, |d| {
            d.set_session_model(&f.session.id, Some("claude-sonnet-4-5"))
        })
        .unwrap();
        with_db(&f.db, |d| d.set_session_effort(&f.session.id, Some("high"))).unwrap();
        user_message(&f.db, &f.session.id, "hi", 2_000);

        finish(begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap()).await;

        let calls = llm.calls();
        assert_eq!(
            calls[0].model, "claude-sonnet-4-5",
            "the pin, not the ctx default"
        );
        assert_eq!(calls[0].effort, Some(Effort::High));
    }

    #[tokio::test]
    async fn with_no_pin_the_ctx_default_wins_and_with_neither_the_built_in_does() {
        let pinned = scripted_llm(vec![ScriptedRound {
            content: vec![text("done"), stop("stop-1")],
            ..Default::default()
        }]);
        let f = fixture(opts(pinned.clone()));
        user_message(&f.db, &f.session.id, "hi", 2_000);
        finish(begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap()).await;
        assert_eq!(pinned.calls()[0].model, "claude-opus-4-8");

        let bare = scripted_llm(vec![ScriptedRound {
            content: vec![text("done"), stop("stop-1")],
            ..Default::default()
        }]);
        let mut o = opts(bare.clone());
        o.model = None;
        let g = fixture(o);
        user_message(&g.db, &g.session.id, "hi", 2_000);
        finish(begin_turn(&g.ctx, &g.session.id, g.deps.clone()).unwrap()).await;
        assert_eq!(bare.calls()[0].model, DEFAULT_MODEL);
    }

    // ---- the workspace note ------------------------------------------------

    #[tokio::test]
    async fn every_turns_prompt_is_told_which_checkout_it_is_editing() {
        // The seam this closes: `PromptInput.notes` and `TurnDeps.notes` both
        // existed and nobody filled either, so the model was never told where
        // `bash` starts or where a relative `view()` path resolves — and the
        // program's own cwd is the SERVER's directory, not the workspace, so
        // guessing wrong is silent and reachable.
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![text("done"), stop("c1")],
            ..Default::default()
        }]);
        let f = fixture(opts(llm));
        with_db(&f.db, |d| {
            d.set_session_workspace(&f.session.id, "/checkouts/acme")
        })
        .unwrap();

        let seen: Arc<Mutex<Option<PromptInput>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let mut deps = f.deps.clone();
        deps.assemble = Some(Arc::new(move |input: &PromptInput| {
            *s.lock().unwrap() = Some(input.clone());
            AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            }
        }));
        user_message(&f.db, &f.session.id, "hi", 2_000);
        finish(begin_turn(&f.ctx, &f.session.id, deps).unwrap()).await;

        let notes = seen.lock().unwrap().as_ref().unwrap().notes.clone();
        assert!(
            !notes.is_empty(),
            "the turn must supply at least the workspace note"
        );
        assert!(
            notes[0].contains("/checkouts/acme"),
            "the workspace note must name the session's checkout, got: {}",
            notes[0]
        );
    }

    /// The seam this closes, and it is the same shape as the workspace note
    /// above: `PromptInput.skills` existed, `active_skills`/`turn_skills`
    /// existed and were tested, and NOTHING CALLED THEM — the field was
    /// hardcoded `vec![]` right here. So a `/name` parsed correctly, listed
    /// correctly in the panel, granted its `mcp:` servers to nobody, and
    /// reached the model as absolutely nothing. Every skill in the product was
    /// inert, and every test of the skills module passed while it was.
    #[tokio::test]
    async fn a_named_skill_reaches_the_prompt_and_grants_its_servers() {
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![text("done"), stop("c1")],
            ..Default::default()
        }]);
        let f = fixture(opts(llm));
        // A project-tier skill, which also pins that `sources_for` looks in
        // the workspace at all.
        let root =
            std::env::temp_dir().join(format!("bough-runner-skill-{}", uuid::Uuid::new_v4()));
        let skill = root.join(".agents").join("skills").join("deploy");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\ndescription: ship it\nmcp: linear\n---\nDEPLOY INSTRUCTIONS",
        )
        .unwrap();
        with_db(&f.db, |d| {
            d.set_session_workspace(&f.session.id, &root.to_string_lossy())
        })
        .unwrap();

        let seen: Arc<Mutex<Option<PromptInput>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let granted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let g = granted.clone();
        let mut deps = f.deps.clone();
        deps.assemble = Some(Arc::new(move |input: &PromptInput| {
            *s.lock().unwrap() = Some(input.clone());
            AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            }
        }));
        deps.mcp_catalog = Some(Arc::new(move |_session: &str, extra: &[String]| {
            *g.lock().unwrap() = extra.to_vec();
            vec![]
        }));
        user_message(&f.db, &f.session.id, "please /deploy now", 2_000);
        finish(begin_turn(&f.ctx, &f.session.id, deps).unwrap()).await;

        let input = seen.lock().unwrap().clone().unwrap();
        assert_eq!(
            input
                .skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["deploy"],
            "the skill the message named must reach the prompt"
        );
        assert!(input.skills[0].body.contains("DEPLOY INSTRUCTIONS"));
        assert_eq!(
            granted.lock().unwrap().clone(),
            ["linear"],
            "a skill's `mcp:` list is its grant, so the catalog must be widened by it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The other half of the intact-or-reported invariant, at the runner
    /// level: a skill the user named and the harness could not parse must
    /// reach the model as a note, not as silence.
    #[tokio::test]
    async fn a_named_but_broken_skill_reaches_the_prompt_as_a_note() {
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![text("done"), stop("c1")],
            ..Default::default()
        }]);
        let f = fixture(opts(llm));
        let root =
            std::env::temp_dir().join(format!("bough-runner-broken-{}", uuid::Uuid::new_v4()));
        let skill = root.join(".agents").join("skills").join("halfwritten");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        // Opens a fence it never closes: the body is withheld by design.
        std::fs::write(skill.join("SKILL.md"), "---\ndescription: oops\n\nthe body").unwrap();
        with_db(&f.db, |d| {
            d.set_session_workspace(&f.session.id, &root.to_string_lossy())
        })
        .unwrap();

        let seen: Arc<Mutex<Option<PromptInput>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let mut deps = f.deps.clone();
        deps.assemble = Some(Arc::new(move |input: &PromptInput| {
            *s.lock().unwrap() = Some(input.clone());
            AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            }
        }));
        user_message(&f.db, &f.session.id, "run /halfwritten", 2_000);
        finish(begin_turn(&f.ctx, &f.session.id, deps).unwrap()).await;

        let input = seen.lock().unwrap().clone().unwrap();
        assert!(
            input.skills.is_empty(),
            "a broken skill contributes no body"
        );
        assert!(
            input
                .notes
                .iter()
                .any(|n| n.contains("Skill /halfwritten could not be loaded")),
            "the model must be told the file is wrong, got: {:?}",
            input.notes
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_callers_own_notes_are_kept_and_the_workspace_note_leads() {
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![text("done"), stop("c1")],
            ..Default::default()
        }]);
        let f = fixture(opts(llm));
        with_db(&f.db, |d| {
            d.set_session_workspace(&f.session.id, "/checkouts/acme")
        })
        .unwrap();

        let seen: Arc<Mutex<Option<PromptInput>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let mut deps = f.deps.clone();
        deps.notes = Some(vec!["## Project rules\n\nno emoji".to_string()]);
        deps.assemble = Some(Arc::new(move |input: &PromptInput| {
            *s.lock().unwrap() = Some(input.clone());
            AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            }
        }));
        user_message(&f.db, &f.session.id, "hi", 2_000);
        finish(begin_turn(&f.ctx, &f.session.id, deps).unwrap()).await;

        let notes = seen.lock().unwrap().as_ref().unwrap().notes.clone();
        // The two per-session facts lead, in the order a turn needs them:
        // where the real work goes, then where the throwaway files go.
        assert!(notes[0].starts_with("## Workspace"));
        assert!(notes[1].starts_with("## Scratchpad"));
        assert!(
            notes[1].contains(&f.session.id),
            "the scratchpad note names THIS session's dir"
        );
        // SEARCHED, not indexed, and no assertion on the total. The rules note
        // sits between the scratchpad and the caller's, and it is built from
        // the REAL `$BOUGH_HOME/AGENTS.md` and `~/.claude/CLAUDE.md` — so a
        // fixed index and a fixed count both make this test pass or fail on
        // whether the developer running it happens to keep those files.
        assert!(
            notes.iter().any(|n| n.contains("no emoji")),
            "a caller's notes must survive: {notes:?}"
        );
    }

    // ---- the tag-history memory wiring (rows 2.9–2.12) ---------------------

    /// Seed one command-history row so the workspace repo has a vocabulary.
    fn seed_history(db: &SharedDb, session_id: &str, repo: &str, tag: &str, dir: Option<&str>) {
        let ts = crate::types::system_clock()();
        with_db(db, |d| {
            d.record_command(&crate::types::CommandRecord {
                session_id: session_id.to_string(),
                ts,
                repo: repo.to_string(),
                cmd: format!("seeded-{tag}-{ts}-{}", Uuid::new_v4()),
                tags: tag.to_string(),
                tag_list: vec![tag.to_string()],
                dirs: dir.map(|d| vec![d.to_string()]).unwrap_or_default(),
                exit_code: Some(0),
                duration_ms: Some(1),
                output_head: String::new(),
                spill_path: None,
                source: "live".into(),
                message_id: None,
            })
        })
        .unwrap();
    }

    fn temp_workspace(tag: &str) -> std::path::PathBuf {
        let ws = std::env::temp_dir().join(format!("bough-runner-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }

    #[tokio::test]
    async fn the_tag_priming_note_rides_the_volatile_tier_between_scratch_and_caller_notes() {
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![text("done"), stop("c1")],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("done"), stop("c2")],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm));
        let ws = temp_workspace("note");
        let ws_s = ws.to_string_lossy().into_owned();
        with_db(&f.db, |d| d.set_session_workspace(&f.session.id, &ws_s)).unwrap();
        // Two uses each — a singleton would be demoted out of the note.
        for _ in 0..2 {
            seed_history(&f.db, &f.session.id, &ws_s, "composer", None);
            seed_history(&f.db, &f.session.id, &ws_s, "retention", None);
        }

        let seen: Arc<Mutex<Option<PromptInput>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let mut deps = f.deps.clone();
        deps.assemble = Some(Arc::new(move |input: &PromptInput| {
            *s.lock().unwrap() = Some(input.clone());
            AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            }
        }));
        user_message(&f.db, &f.session.id, "hi", 2_000);
        finish(begin_turn(&f.ctx, &f.session.id, deps.clone()).unwrap()).await;

        let notes = seen.lock().unwrap().as_ref().unwrap().notes.clone();
        // No count: a rules note follows these three whenever the machine
        // running the test has a global AGENTS.md or ~/.claude/CLAUDE.md.
        assert!(notes[0].starts_with("## Workspace"));
        assert!(notes[1].starts_with("## Scratchpad"));
        assert!(
            notes[2].starts_with("This project's own tag vocabulary"),
            "the priming note rides the volatile tier: {}",
            notes[2]
        );
        assert!(notes[2].contains("composer") && notes[2].contains("retention"));
        // Hints are result-carried, never prompt-carried.
        assert!(
            notes.iter().all(|n| !n.contains("[history]")),
            "no dir hint may reach the prompt: {notes:?}"
        );

        // The note froze per session at first computation: new stats do not
        // drift a later turn of the SAME session.
        for _ in 0..3 {
            seed_history(&f.db, &f.session.id, &ws_s, "drifted", None);
        }
        user_message(&f.db, &f.session.id, "again", 3_000);
        finish(begin_turn(&f.ctx, &f.session.id, deps).unwrap()).await;
        let notes2 = seen.lock().unwrap().as_ref().unwrap().notes.clone();
        assert_eq!(notes2[2], notes[2], "a session's note never drifts");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn a_live_round_records_bash_into_the_memory_and_hints_land_on_the_round_result() {
        // The full wiring, through the REAL sidecar and a REAL shell: the
        // model's program runs `bash("ls migrations", …)`, the recorder
        // writes the row, the touched dir triggers a divergence hint, and the
        // hint lands on the round's RESULT (the tool_result output) — never
        // on the prompt.
        let ws = temp_workspace("hints");
        std::fs::create_dir_all(ws.join("migrations")).unwrap();
        let ws_s = ws.to_string_lossy().into_owned();

        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![run_steps(
                    "call-1",
                    r#"await bash("ls migrations/", "ls:list")"#,
                )],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("done"), stop("stop-1")],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm));
        with_db(&f.db, |d| d.set_session_workspace(&f.session.id, &ws_s)).unwrap();
        // The priming set is {bun}; migrations/ knows a word the session was
        // not primed with.
        for _ in 0..2 {
            seed_history(&f.db, &f.session.id, &ws_s, "bun", None);
        }
        seed_history(&f.db, &f.session.id, &ws_s, "psql", Some("migrations"));

        let seen: Arc<Mutex<Option<PromptInput>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let mut deps = f.deps.clone();
        deps.program = None;
        deps.assemble = Some(Arc::new(move |input: &PromptInput| {
            *s.lock().unwrap() = Some(input.clone());
            AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            }
        }));
        user_message(&f.db, &f.session.id, "look around", 2_000);
        let started = begin_turn(&f.ctx, &f.session.id, deps).unwrap();
        let message = started.message.clone();
        let outcome = finish(started).await;
        assert_eq!(outcome.status, TurnOutcomeStatus::Done);

        // The hint is on the round RESULT…
        let parts = parts_of(&f.db, &message.id);
        let result_text = parts
            .iter()
            .find_map(|p| match p {
                Part::ToolResult { output, .. } => output.as_str().map(str::to_string),
                _ => None,
            })
            .expect("a tool_result part");
        assert!(
            result_text.contains("[history] tags previously used in migrations/"),
            "the dir hint must ride the tool result: {result_text}"
        );
        assert!(result_text.contains("psql"), "{result_text}");
        assert!(
            !result_text.contains("bun"),
            "primed tags never repeat in a hint"
        );
        // …and never on the prompt.
        let notes = seen.lock().unwrap().as_ref().unwrap().notes.clone();
        assert!(notes.iter().all(|n| !n.contains("[history]")), "{notes:?}");

        // The recorder wrote the command, tags normalized, dir attributed.
        let recorded = with_db(&f.db, |d| d.commands_for_tag("ls", Some(&ws_s), None)).unwrap();
        assert_eq!(recorded.len(), 1, "one live bash row");
        assert_eq!(recorded[0].cmd, "ls migrations/");
        assert_eq!(recorded[0].tags, "ls:list");
        assert_eq!(recorded[0].exit_code, Some(0));
        assert_eq!(recorded[0].session_id, f.session.id);

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn what_the_user_asked_recalls_the_command_that_worked_onto_the_round_result() {
        // The other half of the memory: the dir hints answer "where are you",
        // this answers "what did you ask". Same carrier, same reason — the
        // line rides the tool result, because a per-turn prompt edit would
        // bust the volatile tier.
        let ws = temp_workspace("query-hints");
        let ws_s = ws.to_string_lossy().into_owned();

        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![run_steps("call-1", r#"await bash("pwd", "pwd:where")"#)],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("done"), stop("stop-1")],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm));
        with_db(&f.db, |d| d.set_session_workspace(&f.session.id, &ws_s)).unwrap();
        // One use, so it is demoted out of the priming note — exactly the
        // word the static note cannot teach and this channel can.
        seed_history(&f.db, &f.session.id, &ws_s, "retention", None);

        let seen: Arc<Mutex<Option<PromptInput>>> = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let mut deps = f.deps.clone();
        deps.program = None;
        deps.assemble = Some(Arc::new(move |input: &PromptInput| {
            *s.lock().unwrap() = Some(input.clone());
            AssembledPrompt {
                system: "SYSTEM".into(),
                system_volatile: "".into(),
                sections: vec![],
                shas: vec![],
            }
        }));
        user_message(
            &f.db,
            &f.session.id,
            "how does retention pruning work?",
            2_000,
        );
        let started = begin_turn(&f.ctx, &f.session.id, deps).unwrap();
        let message = started.message.clone();
        assert_eq!(finish(started).await.status, TurnOutcomeStatus::Done);

        let parts = parts_of(&f.db, &message.id);
        let result_text = parts
            .iter()
            .find_map(|p| match p {
                Part::ToolResult { output, .. } => output.as_str().map(str::to_string),
                _ => None,
            })
            .expect("a tool_result part");
        assert!(
            result_text.contains("[history] this repo has worked on that before"),
            "the query hint must ride the tool result: {result_text}"
        );
        assert!(result_text.contains("retention"), "{result_text}");
        let notes = seen.lock().unwrap().as_ref().unwrap().notes.clone();
        assert!(notes.iter().all(|n| !n.contains("[history]")), "{notes:?}");

        let _ = std::fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------
    // The note memory's two halves in a round
    // -----------------------------------------------------------------

    fn plain_result() -> ProgramResult {
        ProgramResult {
            ok: true,
            logs: vec!["did a thing".to_string()],
            error: None,
            interrupted: None,
        }
    }

    /// A ctx whose db is shared, with a session id unique per test so the
    /// injection ledger (a process global) cannot leak between them.
    fn note_ctx(db: &crate::types::SharedDb, session: &str) -> TurnCtx {
        crate::notes::resolve::forget(session);
        crate::agents::testkit::turn_ctx_for(db, session, "turn-1", 0)
    }

    /// A note with one section, written the way the CLI writes one.
    fn seed_note(db: &crate::types::SharedDb, path: &str, heading: &str, body: &str) -> i64 {
        let guard = db.lock().unwrap();
        let tags: Vec<String> = path.split(':').map(str::to_string).collect();
        let id = guard.upsert_note(path, path, &tags, 1).unwrap();
        guard
            .put_section(
                &crate::types::SectionWrite {
                    note_id: id,
                    ord: 0,
                    heading: heading.to_string(),
                    body: body.to_string(),
                    tags: None,
                    citations: vec![],
                    author: crate::types::NoteAuthor::Human,
                },
                1,
            )
            .unwrap();
        id
    }

    #[tokio::test]
    async fn a_reference_with_a_note_gets_one_hint_on_the_rounds_result() {
        let db = crate::agents::testkit::shared_db();
        seed_note(
            &db,
            "linear.nme-1673",
            "Executor ordering",
            "DAG removal lands before the swap.",
        );
        let ctx = note_ctx(&db, "s-hint");
        let refs = vec![("linear.nme-1673".to_string(), Some(0))];

        let out = with_note_hint_notes(plain_result(), &ctx, &refs);
        let line = out
            .logs
            .iter()
            .find(|l| l.starts_with("[notes]"))
            .expect("a note hint");
        assert!(
            line.contains("DAG removal lands before the swap."),
            "{line}"
        );

        // THE LEDGER: the same section, unchanged, is already in the context
        // above — so the second round says nothing at all.
        let again = with_note_hint_notes(plain_result(), &ctx, &refs);
        assert!(again.logs.iter().all(|l| !l.starts_with("[notes]")));
    }

    #[tokio::test]
    async fn a_grown_section_re_injects_only_its_new_lines() {
        let db = crate::agents::testkit::shared_db();
        let id = seed_note(&db, "pr.7134", "Rollout", "line one\nline two\nline three");
        let ctx = note_ctx(&db, "s-grow");
        let refs = vec![("pr.7134".to_string(), Some(0))];
        with_note_hint_notes(plain_result(), &ctx, &refs);

        db.lock()
            .unwrap()
            .put_section(
                &crate::types::SectionWrite {
                    note_id: id,
                    ord: 0,
                    heading: "Rollout".into(),
                    body: "line one\nline two\nline three\nline four".into(),
                    tags: None,
                    citations: vec![],
                    author: crate::types::NoteAuthor::Human,
                },
                2,
            )
            .unwrap();

        let out = with_note_hint_notes(plain_result(), &ctx, &refs);
        let line = out.logs.iter().find(|l| l.starts_with("[notes]")).unwrap();
        assert!(line.contains("+1"), "{line}");
        assert!(line.contains("line four"), "{line}");
        assert!(!line.contains("line one"), "already said: {line}");
    }

    #[tokio::test]
    async fn a_reference_with_no_note_changes_the_round_not_at_all() {
        let db = crate::agents::testkit::shared_db();
        let ctx = note_ctx(&db, "s-silent");
        let before = plain_result();
        let after = with_note_hint_notes(
            before.clone(),
            &ctx,
            &[("linear.nothing-1".to_string(), Some(0))],
        );
        assert_eq!(after.logs, before.logs);
    }

    #[tokio::test]
    async fn without_a_cheap_tier_the_fold_is_a_non_event() {
        let db = crate::agents::testkit::shared_db();
        let id = seed_note(&db, "pr.1", "P", "mine");
        let ctx = note_ctx(&db, "s-nocheap");
        assert!(ctx.app.cheap.is_none());

        fold_round_into_notes(&ctx, &[("pr.1".to_string(), Some(0))]).await;
        assert!(db.lock().unwrap().note_log(id, 10).unwrap().is_empty());
    }

    struct Tier(std::sync::Mutex<Vec<String>>, Option<String>);
    #[async_trait::async_trait]
    impl crate::types::CheapTier for Tier {
        async fn title(&self, _f: &str, _glossary: &[String]) -> Option<String> {
            None
        }
        async fn ghost_text(&self, _p: &str) -> Option<String> {
            None
        }
        async fn activity(&self, _r: &str) -> Option<String> {
            None
        }
        async fn note_line(&self, prompt: &str) -> Option<String> {
            self.0.lock().unwrap().push(prompt.to_string());
            self.1.clone()
        }
    }

    #[tokio::test]
    async fn the_fold_appends_one_cheap_line_and_never_touches_the_prose() {
        let db = crate::agents::testkit::shared_db();
        let id = seed_note(&db, "pr.5002", "Rollout", "prose only a human writes");
        let mut ctx = note_ctx(&db, "s-fold");
        let tier = Arc::new(Tier(
            std::sync::Mutex::new(Vec::new()),
            Some("the backfill window is the blocker".into()),
        ));
        ctx.app.cheap = Some(tier.clone());

        fold_round_into_notes(&ctx, &[("pr.5002".to_string(), Some(0))]).await;

        let log = db.lock().unwrap().note_log(id, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].source, crate::types::NoteAuthor::Cheap);
        assert_eq!(log[0].text, "the backfill window is the blocker");
        let sections = db.lock().unwrap().sections_for_note(id).unwrap();
        assert_eq!(sections[0].body, "prose only a human writes");
        assert_eq!(sections[0].author, crate::types::NoteAuthor::Human);

        let prompt = tier.0.lock().unwrap()[0].clone();
        assert!(prompt.contains("pr.5002 — worked"));
        assert!(prompt.contains("prose only a human writes"));
        for leaked in ["exit_code", "output_head", "bash"] {
            assert!(!prompt.contains(leaked), "{leaked} reached the cheap model");
        }
    }

    #[tokio::test]
    async fn a_skip_writes_nothing() {
        let db = crate::agents::testkit::shared_db();
        let id = seed_note(&db, "pr.9", "R", "prose");
        let mut ctx = note_ctx(&db, "s-skip");
        ctx.app.cheap = Some(Arc::new(Tier(std::sync::Mutex::new(Vec::new()), None)));
        fold_round_into_notes(&ctx, &[("pr.9".to_string(), Some(0))]).await;
        assert!(db.lock().unwrap().note_log(id, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_reference_is_folded_once_per_debounce_window_not_once_per_round() {
        // The defect this closes: a turn LOOPS, so a ten-round turn working on
        // one ticket made ten cheap-model calls, each on the critical path.
        // A DISTINCT reference per test: the debounce ledger is a process
        // global, so two tests sharing one would debounce each other.
        let db = crate::agents::testkit::shared_db();
        let id = seed_note(&db, "pr.5001", "Rollout", "prose");
        let mut ctx = note_ctx(&db, "s-debounce");
        let tier = Arc::new(Tier(
            std::sync::Mutex::new(Vec::new()),
            Some("a line".into()),
        ));
        ctx.app.cheap = Some(tier.clone());

        for _ in 0..5 {
            fold_round_into_notes(&ctx, &[("pr.5001".to_string(), Some(0))]).await;
        }
        assert_eq!(
            tier.0.lock().unwrap().len(),
            1,
            "five rounds, one call to the model"
        );
        assert_eq!(db.lock().unwrap().note_log(id, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_debounce_is_per_reference() {
        let db = crate::agents::testkit::shared_db();
        seed_note(&db, "pr.100", "A", "prose");
        seed_note(&db, "pr.200", "B", "prose");
        let mut ctx = note_ctx(&db, "s-two-refs");
        let tier = Arc::new(Tier(
            std::sync::Mutex::new(Vec::new()),
            Some("a line".into()),
        ));
        ctx.app.cheap = Some(tier.clone());
        fold_round_into_notes(
            &ctx,
            &[
                ("pr.100".to_string(), Some(0)),
                ("pr.200".to_string(), Some(0)),
            ],
        )
        .await;
        assert_eq!(tier.0.lock().unwrap().len(), 2, "one call each");
    }

    #[tokio::test]
    async fn the_fold_never_rides_the_rounds_critical_path() {
        // `spawn_note_fold` returns before the model is reached. A tier that
        // blocks forever must not be able to hold a round.
        struct Hang;
        #[async_trait::async_trait]
        impl crate::types::CheapTier for Hang {
            async fn title(&self, _f: &str, _glossary: &[String]) -> Option<String> {
                None
            }
            async fn ghost_text(&self, _p: &str) -> Option<String> {
                None
            }
            async fn activity(&self, _r: &str) -> Option<String> {
                None
            }
            async fn note_line(&self, _p: &str) -> Option<String> {
                futures::future::pending::<()>().await;
                None
            }
        }
        let db = crate::agents::testkit::shared_db();
        seed_note(&db, "pr.999", "Hangs", "prose");
        let mut ctx = note_ctx(&db, "s-hang");
        ctx.app.cheap = Some(Arc::new(Hang));

        let started = std::time::Instant::now();
        spawn_note_fold(&ctx, &[("pr.999".to_string(), Some(0))]);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "spawning must not wait on the model"
        );
    }

    #[tokio::test]
    async fn the_fold_will_not_create_a_page_for_a_reference_that_has_not_earned_one() {
        let db = crate::agents::testkit::shared_db();
        let mut ctx = note_ctx(&db, "s-threshold");
        ctx.app.cheap = Some(Arc::new(Tier(
            std::sync::Mutex::new(Vec::new()),
            Some("something".into()),
        )));
        // No commands under it at all — 143 references on a real memory, and a
        // page for each is an index nobody can read.
        fold_round_into_notes(&ctx, &[("linear.brand-new".to_string(), Some(0))]).await;
        assert!(db
            .lock()
            .unwrap()
            .note_by_path("linear.brand-new")
            .unwrap()
            .is_none());
    }

    #[test]
    fn an_auth_failure_names_the_environment_variable_because_there_is_no_keys_panel() {
        // Walked as a first-run persona with a bad key: the very first screen
        // said "update it in the keys panel". There is no keys panel — keys
        // are environment variables here. A message naming a surface that
        // does not exist is the same defect as a legend naming a key that is
        // not bound, on the one screen where the reader has nothing else to
        // go on.
        let rejected = "invalid x-api-key";
        let missing = "Could not resolve authentication method";

        assert!(friendly_turn_error(rejected, "claude-haiku-4-5").contains("ANTHROPIC_API_KEY"));
        assert!(friendly_turn_error(missing, "claude-haiku-4-5").contains("ANTHROPIC_API_KEY"));
        // Per provider, from the same map the client reads keys with.
        assert!(friendly_turn_error(rejected, "openai:gpt-5").contains("OPENAI_API_KEY"));
        assert!(friendly_turn_error(rejected, "meta/llama-3").contains("OPENROUTER_API_KEY"));
        // And nothing still points at the panel.
        for m in ["claude-haiku-4-5", "openai:gpt-5", "meta/llama-3"] {
            assert!(!friendly_turn_error(rejected, m).contains("keys panel"));
            assert!(!friendly_turn_error(missing, m).contains("keys panel"));
        }
    }

    #[test]
    fn a_command_that_exited_non_zero_is_reported_even_when_the_program_never_printed_it() {
        // Found by a reviewer persona: `await bash("exit 3")` with no
        // console.log produced `◇ run_steps ✓ done` over "(the program ran and
        // printed nothing)", and the model then narrated an invented mechanism
        // — "bash() threw on the non-zero exit code" — which the shell layer
        // explicitly does not do. The harness knew the code the whole time.
        let silent = ProgramResult {
            ok: true,
            logs: vec![],
            error: None,
            interrupted: None,
        };
        let noted = with_exit_notes(
            silent,
            &[ExitNote {
                command: "exit 3".into(),
                code: 3,
            }],
        );
        assert_eq!(noted.logs, vec!["[exit code 3] exit 3"]);

        // A program that DID print it is left alone — saying it twice is its
        // own noise.
        let printed = ProgramResult {
            ok: true,
            logs: vec!["boom\n[exit code 3]".into()],
            error: None,
            interrupted: None,
        };
        assert_eq!(
            with_exit_notes(
                printed.clone(),
                &[ExitNote {
                    command: "exit 3".into(),
                    code: 3
                }]
            )
            .logs,
            printed.logs
        );

        // Nothing failed, nothing appended.
        let fine = ProgramResult {
            ok: true,
            logs: vec!["fine".into()],
            error: None,
            interrupted: None,
        };
        assert_eq!(with_exit_notes(fine, &[]).logs, vec!["fine"]);

        // Several failures are each named, and a long command is clipped onto
        // one line.
        let many = with_exit_notes(
            ProgramResult {
                ok: true,
                logs: vec![],
                error: None,
                interrupted: None,
            },
            &[
                ExitNote {
                    command: "false".into(),
                    code: 1,
                },
                ExitNote {
                    command: format!("echo {}", "x".repeat(200)),
                    code: 2,
                },
            ],
        );
        assert_eq!(many.logs.len(), 2);
        assert!(many.logs[0].starts_with("[exit code 1] false"));
        assert!(many.logs[1].ends_with("…"), "{}", many.logs[1]);
    }

    // ---- queue.test.ts AC 1: interrupt mid-program -------------------------

    #[tokio::test]
    async fn interrupting_mid_program_leaves_a_well_formed_replayable_transcript() {
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![
                text("Starting the build."),
                run_steps("c1", "await bash('make')"),
            ],
            ..Default::default()
        }]);
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));

        let mut o = opts(llm.clone());
        // A program that runs until the turn's cancel fires, then reports what
        // survived — the shape `run_program` produces on an abort.
        o.program = Some(Arc::new(move |run: ProgramRun| {
            let reached_tx = reached_tx.clone();
            async move {
                if let Some(tx) = reached_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                (run.on_log)("compiling…");
                run.cancel.cancelled().await;
                ProgramResult {
                    ok: false,
                    interrupted: Some(true),
                    logs: vec!["compiling…".into()],
                    error: Some(
                        "program interrupted by the user — the 1 line(s) it printed before \
                         stopping are above; anything it had already done still stands"
                            .into(),
                    ),
                }
            }
            .boxed()
        }));
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "build it", now_ms());

        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        reached_rx.await.unwrap();
        assert!(f.registry.is_running(&f.session.id));
        assert!(interrupt_turn(&f.session.id, &f.registry));

        let outcome = finish(started).await;
        assert_eq!(outcome.status, TurnOutcomeStatus::Interrupted);

        let stored = with_db(&f.db, |d| d.get_message(&message.id))
            .unwrap()
            .unwrap();
        assert!(
            !stored.pending,
            "an interrupted message is closed, not left pending"
        );
        assert_eq!(
            part_types(&stored.parts),
            vec!["text", "tool_call", "tool_result", "text"]
        );

        // Every tool_call has its tool_result — the thing that keeps the
        // thread valid.
        let calls: Vec<&str> = stored
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let results: Vec<&str> = stored
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(calls, results);

        match stored
            .parts
            .iter()
            .find(|p| matches!(p, Part::ToolResult { .. }))
            .unwrap()
        {
            Part::ToolResult {
                interrupted,
                output,
                ..
            } => {
                assert_eq!(
                    *interrupted,
                    Some(true),
                    "stopped, which is not the same as failed"
                );
                let out = output.as_str().unwrap();
                assert!(out.contains("compiling…"), "partial output survived: {out}");
                assert!(out.contains("interrupted by the user"), "{out}");
            }
            _ => unreachable!(),
        }

        // The closing note is the stop marker, not a failure marker.
        assert_eq!(
            stored.parts.last(),
            Some(&Part::Text {
                text: "⏹ Stopped.".into()
            })
        );

        let turn = with_db(&f.db, |d| d.turn_for_message(&message.id))
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, TurnStatus::Interrupted);
        assert_eq!(turn.error, None, "an interrupt is not an error");
        assert_eq!(
            with_db(&f.db, |d| d.busy_session_ids()).unwrap().len(),
            0,
            "the session is free"
        );
        assert!(!f.registry.is_running(&f.session.id));

        // No further round was asked for: stop means stop.
        assert_eq!(llm.calls().len(), 1);
        // ...and the raw interrupt is not reported as an error.
        assert_eq!(f.reported.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn an_interrupt_names_the_background_shells_that_survive_it() {
        let llm = scripted_llm(vec![ScriptedRound {
            content: vec![run_steps("c1", "x")],
            ..Default::default()
        }]);
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
        let mut o = opts(llm);
        o.program = Some(Arc::new(move |run: ProgramRun| {
            let reached_tx = reached_tx.clone();
            async move {
                if let Some(tx) = reached_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                run.cancel.cancelled().await;
                ProgramResult {
                    ok: false,
                    interrupted: Some(true),
                    logs: vec![],
                    error: Some("interrupted".into()),
                }
            }
            .boxed()
        }));
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "go", now_ms());
        let mut deps = f.deps.clone();
        deps.surviving_jobs = Some(Arc::new(|_| vec!["bg_1".to_string(), "bg_2".to_string()]));
        let started = begin_turn(&f.ctx, &f.session.id, deps).unwrap();
        let message = started.message.clone();
        reached_rx.await.unwrap();
        f.registry.interrupt(&f.session.id);
        finish(started).await;

        let parts = parts_of(&f.db, &message.id);
        let note = match parts.last().unwrap() {
            Part::Text { text } => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        assert!(note.contains("bg_1, bg_2 still running"), "{note}");
        assert!(note.contains("they survive the interrupt"), "{note}");
    }

    // ---- queue.test.ts AC 2: two rapid messages ----------------------------

    #[tokio::test]
    async fn two_rapid_messages_produce_two_ordered_turns_with_no_loss() {
        // Turn 1 answers the first message; turn 2 answers the second.
        let llm = scripted_llm(vec![
            ScriptedRound {
                content: vec![text("Answering the first."), stop("s1")],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("Answering the second."), stop("s2")],
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm.clone()));

        let drains: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        // Observed rather than replaced: the real recursion still runs, so
        // this test exercises the actual drain path. The deps cell makes the
        // closure self-referential, as the TS closure was.
        let deps_cell: Arc<std::sync::OnceLock<TurnDeps>> = Arc::new(std::sync::OnceLock::new());
        let d = drains.clone();
        let cell = deps_cell.clone();
        let mut deps = f.deps.clone();
        deps.start_next = Some(Arc::new(move |ctx: &AppCtx, session_id: &str| {
            d.lock().unwrap().push(session_id.to_string());
            let _ = begin_turn(ctx, session_id, cell.get().unwrap().clone());
        }));
        let _ = deps_cell.set(deps.clone());

        user_message(&f.db, &f.session.id, "first", now_ms());
        let first = begin_turn(&f.ctx, &f.session.id, deps.clone()).unwrap();
        // The second message lands while turn 1 is in flight: persisted like
        // any other, and NOT started — the server sees a busy session and
        // 202s.
        assert!(with_db(&f.db, |d| d.busy_session_ids())
            .unwrap()
            .contains(&f.session.id));
        user_message(&f.db, &f.session.id, "second", now_ms());
        assert!(with_db(&f.db, |d| crate::turn::queue::has_unanswered_input(
            d,
            &f.session.id
        ))
        .unwrap());

        finish(first).await;
        // The drain fires with the first turn's release; let the second turn
        // finish.
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if with_db(&f.db, |d| d.busy_session_ids()).unwrap().is_empty()
                && llm.calls().len() == 2
            {
                break;
            }
        }

        assert_eq!(
            *drains.lock().unwrap(),
            vec![f.session.id.clone()],
            "exactly one drain, not one per message"
        );
        let calls = llm.calls();
        assert_eq!(calls.len(), 2);

        // Two turns, in order, both finished.
        let turn_rows = with_db(&f.db, |d| d.turns_for_session(&f.session.id)).unwrap();
        assert_eq!(turn_rows.len(), 2);
        assert!(turn_rows.iter().all(|t| t.status == TurnStatus::Done));
        assert_eq!(with_db(&f.db, |d| d.busy_session_ids()).unwrap().len(), 0);

        // The transcript alternates and nothing was dropped.
        let own = with_db(&f.db, |d| d.messages_for(&f.session.id)).unwrap();
        assert_eq!(
            own.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![Role::User, Role::Supervisor, Role::User, Role::Supervisor]
        );
        let first_texts: Vec<String> = own
            .iter()
            .map(|m| match &m.parts[0] {
                Part::Text { text } => text.clone(),
                other => panic!("expected text, got {other:?}"),
            })
            .collect();
        assert_eq!(
            first_texts,
            vec![
                "first",
                "Answering the first.",
                "second",
                "Answering the second."
            ]
        );
        assert!(own.iter().all(|m| !m.pending));

        // Turn 2 saw the queued message; turn 1 could not have.
        let round1 = messages_json(&calls[0]);
        let round2 = messages_json(&calls[1]);
        assert!(round1.contains("first"));
        assert!(
            !round1.contains("second"),
            "the queued message did not race into the live turn"
        );
        assert!(round2.contains("first") && round2.contains("second"));
        assert!(
            round2.find("Answering the first.").unwrap() < round2.find("second").unwrap(),
            "in order"
        );

        // And it stops: nothing is left unanswered, so no third turn starts.
        assert!(
            !with_db(&f.db, |d| crate::turn::queue::has_unanswered_input(
                d,
                &f.session.id
            ))
            .unwrap()
        );
    }

    // ---- queue.test.ts AC 3: the truncated tool call -----------------------

    #[tokio::test]
    async fn a_tool_call_truncated_mid_stream_is_retried_never_executed() {
        // What the stream layer raises rather than falling back to `{}`.
        let truncation = || {
            BoughError::llm(
                "anthropic: run_steps call arrived with no arguments (truncated mid-call)",
            )
        };

        let llm = scripted_llm(vec![
            ScriptedRound {
                throws: Some(truncation()),
                ..Default::default()
            },
            // The re-streamed round lands intact.
            ScriptedRound {
                content: vec![run_steps("c1", "await bash('git status')")],
                ..Default::default()
            },
            ScriptedRound {
                content: vec![text("Clean tree."), stop("stop-1")],
                ..Default::default()
            },
        ]);
        let mut o = opts(llm.clone());
        o.program = Some(Arc::new(|_| {
            async { logs_result(&["nothing to commit"]) }.boxed()
        }));
        let f = fixture(o);
        user_message(&f.db, &f.session.id, "check the tree", now_ms());

        let started = begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap();
        let message = started.message.clone();
        assert_eq!(finish(started).await.status, TurnOutcomeStatus::Done);

        // THE assertion: the program that ran is the one the model actually
        // wrote.
        {
            let programs = f.programs.lock().unwrap();
            assert_eq!(programs.len(), 1, "the truncated call was not executed");
            assert_eq!(programs[0].code, "await bash('git status')");
        }
        assert_eq!(llm.calls().len(), 3, "the round was re-streamed");

        // The retry is announced, so a client drops the partial text it had
        // buffered.
        {
            let events = f.events.lock().unwrap();
            let retries: Vec<&BoughEvent> = events
                .iter()
                .filter(|e| e.r#type == EventType::MessageRetry)
                .collect();
            assert_eq!(retries.len(), 1);
            let data: MessageRetryData = serde_json::from_value(retries[0].data.clone()).unwrap();
            assert_eq!(data.message_id, message.id);
            assert_eq!(data.attempt, 1);
            assert!(
                data.reason.contains("cut off mid-stream"),
                "{}",
                data.reason
            );
            assert!(
                data.reason
                    .contains("rather than executing a truncated program"),
                "{}",
                data.reason
            );
        }

        // Nothing about the retry reached the transcript beyond the real
        // round.
        let parts = parts_of(&f.db, &message.id);
        assert_eq!(part_types(&parts), vec!["tool_call", "tool_result", "text"]);
    }

    #[tokio::test]
    async fn an_exhausted_retry_surfaces_as_a_turn_error_rather_than_an_executed_guess() {
        let truncation = || {
            BoughError::llm("openai: run_steps call has malformed arguments (truncated mid-call)")
        };
        let llm = scripted_llm(vec![
            ScriptedRound {
                throws: Some(truncation()),
                ..Default::default()
            },
            ScriptedRound {
                throws: Some(truncation()),
                ..Default::default()
            },
            ScriptedRound {
                throws: Some(truncation()),
                ..Default::default()
            },
            ScriptedRound {
                throws: Some(truncation()),
                ..Default::default()
            },
        ]);
        let f = fixture(opts(llm.clone()));
        user_message(&f.db, &f.session.id, "go", now_ms());

        let outcome = finish(begin_turn(&f.ctx, &f.session.id, f.deps.clone()).unwrap()).await;
        assert_eq!(outcome.status, TurnOutcomeStatus::Error);
        assert_eq!(f.programs.lock().unwrap().len(), 0, "still never executed");
        assert_eq!(
            llm.calls().len() as u32,
            crate::turn::queue::MAX_ROUND_RETRIES + 1,
            "retries are bounded"
        );
        assert_eq!(with_db(&f.db, |d| d.busy_session_ids()).unwrap().len(), 0);
    }
}
