//! The shell verbs a program calls: `bash`, `sh`, and the four job verbs
//! wired through to the registry in `jobs.rs`. Port of `src/hostfn/shell.ts`
//! (spec: hostfn.md §shell).
//!
//! WHY THIS EXISTS. A program runs as the user with the user's full authority,
//! and could spawn processes directly for any of this. These exist for the
//! three things a raw spawn cannot do: carry the **turn's interrupt**, hand a
//! long command to the **background registry** instead of blocking the round
//! on it, and bound the output that crosses back into the model's context
//! **deterministically**. Nothing here is confinement (spec §2.2).
//!
//! THE INVARIANT THIS HOLDS: **a foreground command never blocks the turn and
//! is never killed for taking too long** (plan §6.7). Past the threshold,
//! `bash` returns "…moved to background as bg_N" and the command KEEPS
//! RUNNING — the model reads it with `bashOutput`, blocks on it with
//! `bashWait`, and is told when it exits. That is the whole reason a program
//! never has to write a sleep/poll loop, so "it timed out, try again" is not
//! an outcome this module is allowed to produce.
//!
//! The second rule, and the reason `sh` is not implemented in terms of `bash`:
//! **`sh` never throws on a non-zero exit.** Its purpose is fanning out
//! commands that are ALLOWED to fail — linters, greps, per-package builds —
//! and inspecting the codes, so the exit code is returned as data, per
//! command, in input order. It also must not auto-background: a backgrounded
//! shell has no exit code yet, and `[{code, out}]` with a missing code is a
//! contract the caller cannot branch on.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::sys::signal::Signal;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::errors::BoughError;
use crate::history::tags::record::{normalize_tags, spill_path_from, OUTPUT_HEAD_CHARS};
use crate::hostfn::jobs::{
    background_note, format_final, shell_text, signal_tree, JobCtx, JobRegistry, PromoteOpts,
    Shell, SpawnOpts,
};
use crate::hostfn::spill::{spill, RealSpillDeps, SpillCtx};
use crate::types::{system_clock, CommandRecorder, ExitNote};

// ---------------------------------------------------------------------------
// What the shell verbs need from a turn
// ---------------------------------------------------------------------------

/// The two directions of the command-history memory (`history/echo`).
/// Optional on the ctx, so a caller without a database — every unit test —
/// simply gets no echo.
#[derive(Clone)]
pub struct EchoHooks {
    /// What the memory already knows about this command, appended to a
    /// failure. Asked BEFORE this run is recorded.
    pub note: Arc<dyn Fn(&str, Option<i64>, &str) -> Option<String> + Send + Sync>,
    /// Returns the skip text for a command that is failing in a loop —
    /// a guarded command is not spawned and not recorded.
    pub guard: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

/// What the shell verbs need from a turn. `TurnCtx` satisfies it
/// structurally, so `hostfn/` still imports nothing from the server crate
/// (plan §3, module boundary rule).
#[derive(Clone, Default)]
pub struct ShellCtx {
    pub session_id: String,
    pub workspace: String,
    /// Where a non-zero exit is recorded so the TRANSCRIPT can say it
    /// happened. `bash()` returns `[exit code N]` as data — the right call, it
    /// is a result to read. But the string goes into the PROGRAM, and a round
    /// that does not log it leaves no trace; the model once narrated a
    /// confident, invented mechanism ("bash() threw on the non-zero exit
    /// code"). The harness must know the code independently.
    pub exits: Option<Arc<Mutex<Vec<ExitNote>>>>,
    /// The turn's interrupt. Absent in unit tests that never interrupt.
    pub cancel: Option<CancellationToken>,
    /// The session's scratchpad, exported to every command as
    /// `$BOUGH_SCRATCH`. The prompt's sentence about a scratchpad reaches the
    /// MODEL; a shell command is text the model composes, and without a
    /// variable to name in it, `--output` goes to /tmp.
    pub scratch: Option<String>,
    /// Where a finished command enters the tag-history memory. Best-effort by
    /// contract — the recorder swallows its own failures.
    pub record: Option<CommandRecorder>,
    /// The memory pushed back: notes below failures, guards before loops.
    pub echo: Option<EchoHooks>,
}

/// Injected seams. Every default is a constant, never a hidden global.
///
/// NOTE (delta from TS): the registry is REQUIRED here rather than defaulting
/// to a module-static — Rust hangs the process-wide instance on `HostState`
/// and threads it explicitly (architecture §4.4).
#[derive(Clone)]
pub struct ShellOptions {
    /// Where background shells live.
    pub registry: Arc<JobRegistry>,
    /// Auto-background threshold for `bash`. Default `default_bg_after_ms()`.
    pub bg_after_ms: Option<u64>,
    /// Per-command wall clock for `sh`. Default `SH_TIMEOUT_MS`.
    pub sh_timeout_ms: Option<u64>,
}

impl ShellOptions {
    pub fn new(registry: Arc<JobRegistry>) -> Self {
        ShellOptions {
            registry,
            bg_after_ms: None,
            sh_timeout_ms: None,
        }
    }
}

/// A foreground command still running this long auto-backgrounds instead of
/// blocking the turn. ~60s only backgrounds genuinely long commands (builds,
/// servers), not the medium ones a program legitimately waits on.
pub const DEFAULT_BG_AFTER_MS: u64 = 60_000;

/// Per-command wall clock for `sh`. Unlike `bash`, `sh` has no background
/// escape hatch — it owes the caller an exit code — so a hung command must
/// not burn the whole program's budget.
pub const SH_TIMEOUT_MS: u64 = 120_000;

/// How long a stopped command gets to flush its pipes before we give up.
const DRAIN_GRACE_MS: u64 = 1_000;

/// The threshold, with an env override for operators. Read once per
/// resolution, and overridden by `ShellOptions::bg_after_ms`, which is what
/// tests use — a test that had to set an environment variable to exercise the
/// handoff would be neither hermetic nor parallel-safe.
pub fn default_bg_after_ms() -> u64 {
    std::env::var("BOUGH_BASH_BG_AFTER_MS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n as u64)
        .unwrap_or(DEFAULT_BG_AFTER_MS)
}

// ---------------------------------------------------------------------------
// Small shared machinery
// ---------------------------------------------------------------------------

enum Raced {
    Exit,
    Timeout,
}

/// Resolve `Exit` when the child finishes, or `Timeout` after `ms`. (Tokio
/// timers never hold the runtime open — the TS unref is free here.)
async fn race_exit(shell: &Shell, ms: u64) -> Raced {
    tokio::select! {
        _ = shell.wait_exit() => Raced::Exit,
        _ = tokio::time::sleep(Duration::from_millis(ms)) => Raced::Timeout,
    }
}

/// Wait for the shell's output streams to finish draining, but no longer than
/// `ms`. Used on the interrupt path, where the partial output is the whole
/// point: returning before the pipes drain would drop the last chunk. Bounded
/// because a child that ignores SIGTERM — or a grandchild dev server that
/// inherited the pipes — must not turn a stop into a hang.
async fn drained(shell: &Shell, ms: u64) {
    let _ = tokio::time::timeout(Duration::from_millis(ms), shell.wait_pumps()).await;
}

/// Make the turn's interrupt reach the whole process TREE, not just the shell.
///
/// `sh -c 'printf x; sleep 60'` does not forward SIGTERM to its foreground
/// child: kill only the shell and `sleep` is reparented holding the inherited
/// stdout pipe — the stop button looks like it worked while the work kept
/// running (plan §6.3). The registry deliberately does not take the signal at
/// spawn time (the tree walk must snapshot descendants before the direct
/// child dies), so the listener lives here.
///
/// The watcher stays armed past a promotion on purpose: an interrupt kills
/// the running program's children (spec §5), and an auto-backgrounded shell
/// is one of them. It disarms itself on natural exit — signalling a dead
/// child's recycled pid is the one way this could reach an unrelated process.
fn kill_tree_on_abort(
    shell: &Arc<Shell>,
    cancel: Option<&CancellationToken>,
) -> Option<tokio::task::JoinHandle<()>> {
    let cancel = cancel?.clone();
    // A listener added to an ALREADY-cancelled token would in TS never fire;
    // handle it explicitly — nothing else would kill this shell.
    if cancel.is_cancelled() {
        signal_tree(shell, Signal::SIGTERM);
        return None;
    }
    let sh = shell.clone();
    Some(tokio::spawn(async move {
        tokio::select! {
            _ = cancel.cancelled() => signal_tree(&sh, Signal::SIGTERM),
            _ = sh.wait_exit() => {} // finished naturally: detach
        }
    }))
}

/// Spec §6 requires an interrupt to say WHICH stop happened and what
/// survived. A bare "killed" would leave the next round unable to tell an
/// interrupt from a crash.
fn interrupted_error(command: &str) -> BoughError {
    BoughError::program(format!(
        "command killed: the turn was interrupted by the user — `{}` did not finish. \
         Anything it had already done (files written, commands run) still stands; nothing \
         was rolled back.",
        take_chars(command, 80),
    ))
}

/// Append the memory's note to a command's output, if there is one. Below the
/// output rather than above it: the command's own result is what was asked
/// for. A blank line keeps the two from reading as one message.
fn with_echo(out: String, echo: Option<String>) -> String {
    match echo {
        None => out,
        Some(e) if out.is_empty() => e,
        Some(e) => format!("{out}\n\n{e}"),
    }
}

fn is_cancelled(ctx: &ShellCtx) -> bool {
    ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled())
}

fn spawn_opts_for(ctx: &ShellCtx) -> SpawnOpts {
    SpawnOpts {
        cwd: Some(ctx.workspace.clone()),
        scratch: ctx.scratch.clone(),
        session_id: (!ctx.session_id.is_empty()).then(|| ctx.session_id.clone()),
    }
}

fn job_ctx_of(ctx: &ShellCtx) -> JobCtx {
    JobCtx {
        session_id: ctx.session_id.clone(),
        workspace: ctx.workspace.clone(),
    }
}

/// The first `n` chars of `s` (TS `slice` counted UTF-16 units; the two agree
/// on ASCII, which commands overwhelmingly are).
fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `ms / 1000` rendered the way JS string interpolation renders the number:
/// `200 → "0.2"`, `120000 → "120"`.
fn secs_text(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    if s == s.trunc() {
        format!("{}", s as u64)
    } else {
        format!("{s}")
    }
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

/// Run one command with `sh -c` in the session workspace and return its
/// combined output. A non-zero exit is reported in the output as
/// `[exit code N]`, not thrown — it is a result to read, not an error to
/// retry blind.
///
/// Past `bg_after_ms` the running child is handed to the background registry
/// and this returns the handoff note. **The command is not killed and not
/// restarted**; it keeps running under the id in the note.
pub async fn bash(
    command: &str,
    ctx: &ShellCtx,
    opts: &ShellOptions,
    tags: &str,
) -> Result<String, BoughError> {
    let registry = &opts.registry;
    let bg_after_ms = opts.bg_after_ms.unwrap_or_else(default_bg_after_ms);
    // Already stopped: spawning here would produce a process nobody waits on.
    if is_cancelled(ctx) {
        return Err(interrupted_error(command));
    }
    // Nothing is spawned, nothing is recorded — a command that did not run
    // must not enter the memory as if it had, least of all as another failure
    // of itself.
    if let Some(echo) = &ctx.echo {
        if let Some(skipped) = (echo.guard)(command) {
            return Ok(skipped);
        }
    }
    let now = system_clock();
    let started_at = now();

    // Streamed rather than collected, so a long command can be handed to the
    // registry mid-run instead of being blocked on and then killed.
    let shell = registry
        .spawn(command, spawn_opts_for(ctx))
        .map_err(|e| BoughError::program(format!("could not start command: {e}")))?;
    let untrack = registry.track_foreground(&shell, &ctx.session_id);
    let mut kill_task = kill_tree_on_abort(&shell, ctx.cancel.as_ref());

    let result = async {
        match race_exit(&shell, bg_after_ms).await {
            Raced::Exit => {
                if let Some(t) = kill_task.take() {
                    t.abort(); // detach: the child is gone
                }
                // Bounded: the process is gone, so its pipes flush immediately
                // — unless a grandchild it backgrounded inherited them, and a
                // finished command must not become an unbounded wait on
                // somebody else's dev server.
                drained(&shell, DRAIN_GRACE_MS).await;
                if is_cancelled(ctx) {
                    return Err(interrupted_error(command));
                }
                // RECORDED BEFORE IT IS RETURNED. See `ShellCtx::exits`: the
                // string below goes into the program, and a program that does
                // not log it leaves the failure invisible.
                let code = shell.status().map(|s| s.code).unwrap_or(0);
                if code != 0 {
                    if let Some(exits) = &ctx.exits {
                        exits.lock().unwrap().push(ExitNote {
                            command: command.to_string(),
                            code,
                        });
                    }
                }
                // The memory keeps what the PROGRAM saw — head, spill marker
                // and all — so recall can answer "what did it print".
                let final_text = format_final(&shell, &RealSpillDeps);
                // Asked BEFORE this run is recorded, so "already failed 3×"
                // means three times before this one.
                let echo = ctx
                    .echo
                    .as_ref()
                    .and_then(|e| (e.note)(command, Some(code), &final_text));
                if let Some(record) = &ctx.record {
                    record(crate::types::RecordedCommand {
                        command: command.to_string(),
                        tags: tags.to_string(),
                        exit_code: Some(code),
                        duration_ms: Some(now() - started_at),
                        output_head: take_chars(&final_text, OUTPUT_HEAD_CHARS).to_string(),
                        spill_path: spill_path_from(&final_text),
                    });
                }
                Ok(with_echo(final_text, echo))
            }
            Raced::Timeout => {
                // Still running at the threshold. Stopped mid-wait dies like
                // any interrupt.
                if is_cancelled(ctx) {
                    drained(&shell, DRAIN_GRACE_MS).await;
                    return Err(interrupted_error(command));
                }
                // Hand the running child to the registry. Auto-background
                // never kills, and the concurrency cap exists to brake bashBg
                // loops, not to punish a command for being slow — so this
                // promotion always succeeds (force).
                let id = registry
                    .promote(
                        &shell,
                        &job_ctx_of(ctx),
                        PromoteOpts {
                            force: true,
                            name: None,
                        },
                    )
                    .expect("forced promote always succeeds");
                // The memory row waits for the REAL exit: a backgrounded
                // build that fails ten minutes from now must not be
                // remembered as a success. Fire-and-forget — the turn has
                // moved on, and the recorder swallows its own failures.
                if let Some(record) = ctx.record.clone() {
                    let sh = shell.clone();
                    let command = command.to_string();
                    let tags = tags.to_string();
                    let now = now.clone();
                    tokio::spawn(async move {
                        let status = sh.wait_exit().await;
                        // The retained buffer at exit, not `format_final` — a
                        // promoted shell's final rendering belongs to whoever
                        // reads the job; the head is enough here.
                        record(crate::types::RecordedCommand {
                            command,
                            tags,
                            exit_code: Some(status.code),
                            duration_ms: Some(now() - started_at),
                            output_head: take_chars(&shell_text(&sh), OUTPUT_HEAD_CHARS)
                                .to_string(),
                            spill_path: None,
                        });
                    });
                }
                Ok(background_note(&shell, &id, bg_after_ms))
            }
        }
    }
    .await;
    untrack();
    result
}

// ---------------------------------------------------------------------------
// sh
// ---------------------------------------------------------------------------

/// One command's outcome. The code is DATA — `sh` never throws for a non-zero
/// one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShResult {
    pub code: i64,
    pub out: String,
}

/// One `sh` leg: a bare command runs untagged; `{cmd, tag}` stamps the
/// history row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShCommand {
    Plain(String),
    Tagged { cmd: String, tag: Option<String> },
}

/// Run `commands` CONCURRENTLY, one `{code, out}` per command in input order.
///
/// Overlap is real, not simulated: nothing here serializes the shells, and
/// the bridge awaits each host call independently, so N subprocesses run at
/// once.
///
/// This never rejects. A spawn failure, the per-command deadline, and the
/// turn's interrupt all come back as an ordinary result with an explanatory
/// `out`, because the caller asked for a batch of outcomes and losing the
/// other N-1 to one thrown error is never the right answer.
pub async fn sh_concurrent(
    commands: &[ShCommand],
    ctx: &ShellCtx,
    opts: &ShellOptions,
) -> Vec<ShResult> {
    let legs: Vec<(String, String)> = commands
        .iter()
        .map(|c| match c {
            ShCommand::Plain(cmd) => (cmd.clone(), String::new()),
            ShCommand::Tagged { cmd, tag } => (cmd.clone(), normalize_tags(tag.as_deref())),
        })
        .collect();
    futures::future::join_all(
        legs.into_iter()
            .map(|(command, tags)| sh_leg(command, tags, ctx, opts)),
    )
    .await
}

async fn sh_leg(command: String, tags: String, ctx: &ShellCtx, opts: &ShellOptions) -> ShResult {
    let registry = &opts.registry;
    let timeout_ms = opts.sh_timeout_ms.unwrap_or(SH_TIMEOUT_MS);
    let now = system_clock();
    let started_at = now();
    let shell = match registry.spawn(&command, spawn_opts_for(ctx)) {
        Ok(s) => s,
        // Spawn failure (no /bin/sh). Reported, not thrown.
        Err(err) => {
            return ShResult {
                code: -1,
                out: format!("could not start command: {err}"),
            }
        }
    };
    // The deadline: `sh` owes the caller an exit code, so past it the tree is
    // SIGKILLed rather than handed to the background registry.
    let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let timer = {
        let sh = shell.clone();
        let timed_out = timed_out.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
            timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
            signal_tree(&sh, Signal::SIGKILL);
        })
    };
    // `sh -c` does not forward SIGTERM to its foreground child, so the turn's
    // interrupt has to reach the tree the same way the deadline does.
    let kill_task = kill_tree_on_abort(&shell, ctx.cancel.as_ref());

    let status = shell.wait_exit().await;
    if let Some(t) = kill_task {
        t.abort();
    }
    drained(&shell, DRAIN_GRACE_MS).await;
    timer.abort();

    // Retention already bounded the buffer; bound it again so the same rule
    // applies to a command whose output arrived in one burst — and so an
    // oversized `sh` leg spills to a file exactly like an oversized `bash`.
    let sctx = SpillCtx {
        scratch: ctx.scratch.clone(),
        label: Some("sh".to_string()),
    };
    let mut out = spill(&shell_text(&shell), &sctx, None, &RealSpillDeps)
        .trim_end()
        .to_string();
    if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
        out = format!(
            "[killed after {}s — sh has no background handoff; use bashBg(name, cmd) for a \
             command that needs to keep running]\n{out}",
            secs_text(timeout_ms),
        )
        .trim_end()
        .to_string();
    } else if is_cancelled(ctx) {
        out = format!("[the turn was interrupted; this command was killed]\n{out}")
            .trim_end()
            .to_string();
    }
    // A `{cmd, tag}` leg stamps its own history row; a bare string runs
    // untagged. Recorded from the FINAL text, so the memory keeps exactly
    // what the program saw — spill marker included.
    // Noted but never guarded: `sh` legs run concurrently, so the count a
    // guard would read is racing its own siblings. A loop is a `bash` shape
    // anyway.
    let echo = ctx
        .echo
        .as_ref()
        .and_then(|e| (e.note)(&command, Some(status.code), &out));
    if let Some(record) = &ctx.record {
        record(crate::types::RecordedCommand {
            command: command.clone(),
            tags,
            exit_code: Some(status.code),
            duration_ms: Some(now() - started_at),
            output_head: take_chars(&out, OUTPUT_HEAD_CHARS).to_string(),
            spill_path: spill_path_from(&out),
        });
    }
    ShResult {
        code: status.code,
        out: with_echo(out, echo),
    }
}

// ---------------------------------------------------------------------------
// The bridged surface
// ---------------------------------------------------------------------------

/// The six shell host functions, bound to one turn. String-in/string-out
/// because the bridge wire is (`harness/protocol`); the worker side
/// re-inflates the JSON, so a program still writes `await sh("a", "b")` and
/// gets `[{code, out}, …]` — the serialization is invisible to it and lives
/// entirely at this boundary.
pub struct ShellHostFns {
    ctx: ShellCtx,
    opts: ShellOptions,
}

/// Wire the shell verbs for one turn.
pub fn create_shell_host_fns(ctx: ShellCtx, opts: ShellOptions) -> ShellHostFns {
    ShellHostFns { ctx, opts }
}

impl ShellHostFns {
    /// Tags are REQUIRED here, at the boundary, not inside `bash()` —
    /// internal callers and tests drive `bash()` directly and owe no tags;
    /// the MODEL does. The error is a catchable ProgramError that restates
    /// the format, so a model that forgot self-repairs on the next call
    /// instead of abandoning the round.
    pub async fn bash(&self, cmd: &str, tags: Option<&str>) -> Result<String, BoughError> {
        let normalized = normalize_tags(tags);
        if normalized.is_empty() {
            return Err(BoughError::program(
                "bash(cmd, tags) requires tags: 3-5 lowercase tags, colon-separated, naming \
                 the tool, the intent and the subject — e.g. bash(\"git push origin main\", \
                 \"git:push:main\") or bash(\"psql -f migrations/004.sql\", \
                 \"psql:migrate:demand\"). They index this command in your cross-session \
                 history — run `bough tags show <tag>` to read it back.",
            ));
        }
        bash(cmd, &self.ctx, &self.opts, &normalized).await
    }

    /// The bridge is string-only, so `sh` receives a JSON array. Parsing it
    /// is a boundary, and a boundary gets a schema: a model that sends
    /// `sh("ls")` instead of `sh(["ls"])` must be told exactly that. An
    /// element is a bare command string or `{cmd, tag}` — the tagged form the
    /// history records per leg.
    pub async fn sh(&self, cmds_json: &str) -> Result<String, BoughError> {
        let raw: serde_json::Value = serde_json::from_str(cmds_json).map_err(|_| {
            BoughError::program(
                "sh expects a JSON array of command strings; got something that is not JSON. \
                 Call it as sh(\"cmd one\", \"cmd two\").",
            )
        })?;
        let commands = parse_sh_commands(&raw).ok_or_else(|| sh_shape_error(&raw))?;
        let results = sh_concurrent(&commands, &self.ctx, &self.opts).await;
        Ok(serde_json::to_string(&results).expect("ShResult serializes"))
    }

    pub fn bash_bg(&self, name: &str, cmd: &str) -> Result<String, BoughError> {
        self.opts
            .registry
            .bash_bg(name, cmd, &job_ctx_of(&self.ctx), true)
    }

    pub fn bash_output(&self, id: &str) -> Result<String, BoughError> {
        self.opts.registry.bash_output(id, &self.ctx.session_id)
    }

    pub async fn bash_wait(&self, id: &str) -> Result<String, BoughError> {
        self.opts.registry.bash_wait(id, &self.ctx.session_id).await
    }

    pub async fn bash_kill(&self, id: &str) -> Result<String, BoughError> {
        self.opts.registry.bash_kill(id, &self.ctx.session_id).await
    }
}

/// Zod's `array(union(string, object({cmd: string, tag: optional string})))`,
/// by hand: strings pass, `{cmd, tag?}` objects pass (unknown keys stripped;
/// a null tag fails like zod's optional does), anything else fails.
fn parse_sh_commands(raw: &serde_json::Value) -> Option<Vec<ShCommand>> {
    let arr = raw.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        match v {
            serde_json::Value::String(s) => out.push(ShCommand::Plain(s.clone())),
            serde_json::Value::Object(map) => {
                let cmd = map.get("cmd")?.as_str()?.to_string();
                let tag = match map.get("tag") {
                    None => None,
                    Some(serde_json::Value::String(t)) => Some(t.clone()),
                    Some(_) => return None,
                };
                out.push(ShCommand::Tagged { cmd, tag });
            }
            _ => return None,
        }
    }
    Some(out)
}

fn sh_shape_error(raw: &serde_json::Value) -> BoughError {
    let got = if raw.is_array() {
        "an array with an element that is neither"
    } else {
        js_typeof(raw)
    };
    BoughError::program(format!(
        "sh expects command strings or {{cmd, tag}} objects; got {got}. Call it as \
         sh(\"cmd one\", \"cmd two\"), or tag legs for your command history: \
         sh([{{cmd: \"git push\", tag: \"git:push\"}}, \"untagged cmd\"]).",
    ))
}

/// JS `typeof`, for the shape error's "got …" clause (`typeof null` is
/// famously `"object"`).
fn js_typeof(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "object",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/hostfn/shell.test.ts. These run REAL subprocesses,
// which is the only way to test the thing that matters: the auto-background
// handoff is a race between a live child and a threshold, and a fake process
// cannot lose that race the way a real one does. They are still hermetic —
// no network, no ~/.bough — because every command is a /bin/sh builtin or a
// temp-directory file, and every registry is constructed per test.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use crate::hostfn::jobs::{descendant_pids, JobRegistryOptions};
    use crate::hostfn::spill::TruncateLimits;
    use crate::schema::events::{BoughEvent, EventType};
    use crate::schema::parts::{BackgroundJob, JobStatus};
    use crate::types::RecordedCommand;
    use std::collections::HashSet;

    // -- assertion helpers ---------------------------------------------------

    fn has(haystack: &str, needle: &str) {
        assert!(
            haystack.contains(needle),
            "expected to contain {needle:?}, got:\n{haystack}"
        );
    }

    fn lacks(haystack: &str, needle: &str, why: &str) {
        assert!(
            !haystack.contains(needle),
            "{why} — but found {needle:?} in:\n{haystack}"
        );
    }

    fn err_of(r: Result<String, BoughError>) -> BoughError {
        match r {
            Ok(out) => panic!("expected a rejection, but the call resolved:\n{out}"),
            Err(e) => e,
        }
    }

    // -- harness -------------------------------------------------------------

    struct Rig {
        registry: Arc<JobRegistry>,
        ctx: ShellCtx,
        events: Arc<Mutex<Vec<BoughEvent>>>,
        notes: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Rig {
        fn opts(&self) -> ShellOptions {
            ShellOptions::new(self.registry.clone())
        }

        fn opts_bg(&self, bg_after_ms: u64) -> ShellOptions {
            ShellOptions {
                bg_after_ms: Some(bg_after_ms),
                ..self.opts()
            }
        }

        fn event_types(&self) -> Vec<EventType> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.r#type)
                .collect()
        }

        /// SIGTERM anything still running and wait, so no test leaks a process.
        async fn cleanup(&self) {
            self.registry.kill_all();
            self.registry.drain().await;
        }
    }

    fn rig() -> Rig {
        rig_with("sess-1", None)
    }

    fn rig_with(session_id: &str, cancel: Option<CancellationToken>) -> Rig {
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let notes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let bus = Arc::new(Bus::new(system_clock()));
        {
            let events = events.clone();
            bus.subscribe(Arc::new(move |e: &BoughEvent| {
                events.lock().unwrap().push(e.clone())
            }));
        }
        let notify = {
            let notes = notes.clone();
            Arc::new(move |sid: &str, text: &str| {
                notes
                    .lock()
                    .unwrap()
                    .push((sid.to_string(), text.to_string()));
            })
        };
        let registry = Arc::new(JobRegistry::with_options(JobRegistryOptions {
            bus: Some(bus),
            notify: Some(notify),
            ..Default::default()
        }));
        let ctx = ShellCtx {
            session_id: session_id.to_string(),
            workspace: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            cancel,
            ..Default::default()
        };
        Rig {
            registry,
            ctx,
            events,
            notes,
        }
    }

    /// Whether `pid` still exists. Signal 0 tests existence without delivering.
    fn alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    /// Poll `pred` until it holds, or fail after `ms`.
    async fn until_true(what: &str, mut pred: impl FnMut() -> bool, ms: u64) {
        let deadline = std::time::Instant::now() + Duration::from_millis(ms);
        while std::time::Instant::now() < deadline {
            if pred() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// Poll a CURSOR read (`bash_output`) until the text seen across calls
    /// satisfies `check`, and return everything seen. Accumulating is the
    /// point: each call consumes what it returns.
    async fn until_accrued(
        what: &str,
        mut read: impl FnMut() -> String,
        check: impl Fn(&str) -> bool,
        ms: u64,
    ) -> String {
        let deadline = std::time::Instant::now() + Duration::from_millis(ms);
        let mut seen = String::new();
        while std::time::Instant::now() < deadline {
            seen.push_str(&read());
            if check(&seen) {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}; saw:\n{seen}");
    }

    /// Print 200 numbered lines without depending on `seq`.
    const MANY_LINES: &str =
        "i=1; while [ $i -le 200 ]; do printf 'line%s\\n' $i; i=$((i+1)); done";

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A ctx that collects recorder rows.
    fn recording(ctx: &ShellCtx) -> (ShellCtx, Arc<Mutex<Vec<RecordedCommand>>>) {
        let recorded: Arc<Mutex<Vec<RecordedCommand>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = recorded.clone();
        let ctx = ShellCtx {
            record: Some(Arc::new(move |e| sink.lock().unwrap().push(e))),
            ..ctx.clone()
        };
        (ctx, recorded)
    }

    // -- bash: the auto-background handoff (the headline AC) ------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_auto_backgrounds_a_long_command_and_the_job_stays_readable() {
        let r = rig();
        // Prints once before the threshold, once well after, then holds open.
        let out = bash(
            "printf 'before\\n'; sleep 1.5; printf 'after\\n'; sleep 60",
            &r.ctx,
            &r.opts_bg(150),
            "",
        )
        .await
        .unwrap();

        // The handoff note: the id, that it KEEPS RUNNING, the three verbs.
        has(&out, "moved to background as bg_1");
        has(&out, "It keeps running");
        has(&out, "bashOutput(\"bg_1\")");
        has(&out, "bashWait(\"bg_1\")");
        has(&out, "bashKill(\"bg_1\")");
        // Output produced before the handoff rides along, not lost.
        has(&out, "before");

        // The command really is still running; later output is readable.
        let seen = until_accrued(
            "post-handoff output",
            || r.registry.bash_output("bg_1", &r.ctx.session_id).unwrap(),
            |s| s.contains("after"),
            10_000,
        )
        .await;
        has(&seen, "[running]");
        lacks(
            &seen,
            "before",
            "the cursor must not hand the same output out twice",
        );

        let jobs = r.registry.list_jobs(&r.ctx.session_id);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Running);
        assert_eq!(jobs[0].id, "bg_1");
        assert_eq!(r.event_types(), vec![EventType::JobSpawned]);

        let killed = r
            .registry
            .bash_kill("bg_1", &r.ctx.session_id)
            .await
            .unwrap();
        assert!(killed.starts_with("killed bg_1 ("), "{killed}");
        assert_eq!(
            r.event_types(),
            vec![EventType::JobSpawned, EventType::JobExited]
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_auto_backgrounded_command_is_never_killed_by_the_threshold() {
        let r = rig();
        bash("sleep 60", &r.ctx, &r.opts_bg(100), "").await.unwrap();
        // Well past the threshold, the process is still alive.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(r.registry.running_ids(&r.ctx.session_id), vec!["bg_1"]);
        has(
            &r.registry.bash_output("bg_1", &r.ctx.session_id).unwrap(),
            "[running]",
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auto_background_ignores_the_concurrency_cap_so_no_command_is_ever_lost() {
        // The cap brakes bashBg loops. A foreground command that merely took a
        // while must still be handed over rather than blocked-then-killed.
        let r = rig();
        let jctx = job_ctx_of(&r.ctx);
        for _ in 0..8 {
            r.registry
                .bash_bg("sleeper", "sleep 60", &jctx, true)
                .unwrap();
        }
        assert_eq!(r.registry.running_ids(&r.ctx.session_id).len(), 8);

        let out = bash("sleep 60", &r.ctx, &r.opts_bg(100), "").await.unwrap();
        has(&out, "moved to background as bg_9");
        assert_eq!(r.registry.running_ids(&r.ctx.session_id).len(), 9);
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_returns_output_inline_when_the_command_finishes_first() {
        let r = rig();
        assert_eq!(
            bash("printf 'hi\\n'", &r.ctx, &r.opts_bg(5_000), "")
                .await
                .unwrap(),
            "hi"
        );
        assert!(r.registry.list_jobs(&r.ctx.session_id).is_empty());
        assert!(r.events.lock().unwrap().is_empty());
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_reports_a_non_zero_exit_as_data_not_as_a_throw() {
        let r = rig();
        let out = bash("printf 'nope\\n'; exit 3", &r.ctx, &r.opts_bg(5_000), "")
            .await
            .unwrap();
        assert_eq!(out, "nope\n[exit code 3]");
        assert_eq!(
            bash("exit 0", &r.ctx, &r.opts(), "").await.unwrap(),
            "(no output)"
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_non_zero_exit_is_pushed_onto_ctx_exits_before_the_string_returns() {
        let r = rig();
        let exits: Arc<Mutex<Vec<ExitNote>>> = Arc::new(Mutex::new(Vec::new()));
        let ctx = ShellCtx {
            exits: Some(exits.clone()),
            ..r.ctx.clone()
        };
        let out = bash("exit 3", &ctx, &r.opts_bg(5_000), "").await.unwrap();
        assert_eq!(out, "[exit code 3]");
        assert_eq!(
            *exits.lock().unwrap(),
            vec![ExitNote {
                command: "exit 3".to_string(),
                code: 3
            }]
        );
        // A clean exit records nothing.
        bash("exit 0", &ctx, &r.opts_bg(5_000), "").await.unwrap();
        assert_eq!(exits.lock().unwrap().len(), 1);
        r.cleanup().await;
    }

    // -- bash: the turn's interrupt -------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_on_an_already_interrupted_turn_fails_without_spawning_anything() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let r = rig_with("sess-1", Some(cancel));
        let err = err_of(bash("printf hi", &r.ctx, &r.opts(), "").await);
        assert_eq!(err.name(), "ProgramError");
        // Spec §6: name WHICH stop happened, and what survived it.
        has(&err.to_string(), "the turn was interrupted");
        has(&err.to_string(), "still stands");
        assert!(r.registry.list_jobs(&r.ctx.session_id).is_empty());
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn interrupting_a_running_bash_kills_the_child_and_keeps_its_partial_output() {
        let cancel = CancellationToken::new();
        let r = rig_with("sess-1", Some(cancel.clone()));
        let running = tokio::spawn({
            let ctx = r.ctx.clone();
            let opts = r.opts_bg(30_000);
            async move { bash("printf 'partial\\n'; sleep 60", &ctx, &opts, "").await }
        });
        until_true(
            "in-flight foreground output",
            || {
                r.registry
                    .inflight_foreground_output(&r.ctx.session_id)
                    .unwrap_or_default()
                    .contains("partial")
            },
            10_000,
        )
        .await;
        // What the turn runner attaches to the interrupted tool record.
        let partial = r
            .registry
            .inflight_foreground_output(&r.ctx.session_id)
            .unwrap();
        has(&partial, "[interrupted] bash");
        has(&partial, "partial");

        cancel.cancel();
        let err = err_of(running.await.unwrap());
        has(&err.to_string(), "the turn was interrupted");
        // The foreground set empties once the call returns.
        assert_eq!(
            r.registry.inflight_foreground_output(&r.ctx.session_id),
            None
        );
        r.cleanup().await;
    }

    /// THE ONE THAT MATTERS: the interrupt reaches the GRANDCHILD, not just
    /// `sh`. `sh -c 'sleep 47'` does not forward SIGTERM, so killing the
    /// shell alone reparents `sleep` onto init and it runs to completion —
    /// while the TUI prints "interrupting". A rejected future is not a dead
    /// process, so this one asserts on `ps`.
    #[tokio::test(flavor = "multi_thread")]
    async fn interrupting_a_bash_kills_the_grandchild_too_not_just_the_shell() {
        let cancel = CancellationToken::new();
        let r = rig_with("sess-1", Some(cancel.clone()));
        let self_pid = std::process::id() as i32;
        // Diffed against a before-snapshot so only THIS command's processes
        // are asserted on — other tests' shells may be in flight.
        let before: HashSet<i32> = descendant_pids(self_pid).into_iter().collect();
        let running = tokio::spawn({
            let ctx = r.ctx.clone();
            let opts = r.opts_bg(30_000);
            async move { bash("sleep 47; echo never", &ctx, &opts, "").await }
        });
        // Two deep: `sh -c` and the `sleep` it does not forward signals to.
        until_true(
            "the shell and its sleep to appear",
            || {
                descendant_pids(self_pid)
                    .iter()
                    .filter(|p| !before.contains(p))
                    .count()
                    >= 2
            },
            10_000,
        )
        .await;
        let spawned: Vec<i32> = descendant_pids(self_pid)
            .into_iter()
            .filter(|p| !before.contains(p))
            .collect();

        cancel.cancel();
        let err = err_of(running.await.unwrap());
        has(&err.to_string(), "the turn was interrupted");
        until_true(
            "every pid of the interrupted command to die",
            || spawned.iter().all(|&pid| !alive(pid)),
            5_000,
        )
        .await;
        r.cleanup().await;
    }

    // -- sh -------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn sh_never_throws_on_a_non_zero_exit_and_returns_codes_in_input_order() {
        let r = rig();
        let cmds: Vec<ShCommand> = [
            "exit 3",
            "printf 'ok\\n'",
            "exit 1",
            "printf 'err\\n' >&2; exit 7",
        ]
        .iter()
        .map(|c| ShCommand::Plain(c.to_string()))
        .collect();
        let res = sh_concurrent(&cmds, &r.ctx, &r.opts()).await;
        assert_eq!(
            res,
            vec![
                ShResult {
                    code: 3,
                    out: String::new()
                },
                ShResult {
                    code: 0,
                    out: "ok".to_string()
                },
                ShResult {
                    code: 1,
                    out: String::new()
                },
                ShResult {
                    code: 7,
                    out: "err".to_string()
                },
            ]
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sh_reports_a_command_that_does_not_exist_rather_than_throwing() {
        let r = rig();
        let res = sh_concurrent(
            &[ShCommand::Plain(
                "definitely-not-a-command-xyzzy".to_string(),
            )],
            &r.ctx,
            &r.opts(),
        )
        .await;
        assert_eq!(res.len(), 1);
        assert_ne!(
            res[0].code, 0,
            "a missing command must report a non-zero code"
        );
        assert!(
            !res[0].out.is_empty(),
            "the shell's own diagnostic must reach the caller"
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sh_runs_its_commands_concurrently() {
        // A rendezvous rather than a stopwatch: each command creates its own
        // marker and then blocks until the other's exists. Both can only
        // finish if they overlap; serialized, the first would spin until the
        // deadline killed it.
        let dir = temp_dir("bough-sh");
        let r = rig();
        let meet = |mine: &str, theirs: &str| {
            format!(
                "touch {d}/{mine}; while [ ! -f {d}/{theirs} ]; do sleep 0.02; done; \
                 printf '{mine}\\n'",
                d = dir.display(),
            )
        };
        let res = sh_concurrent(
            &[
                ShCommand::Plain(meet("a", "b")),
                ShCommand::Plain(meet("b", "a")),
            ],
            &r.ctx,
            &ShellOptions {
                sh_timeout_ms: Some(10_000),
                ..r.opts()
            },
        )
        .await;
        assert_eq!(
            res,
            vec![
                ShResult {
                    code: 0,
                    out: "a".to_string()
                },
                ShResult {
                    code: 0,
                    out: "b".to_string()
                },
            ]
        );
        r.cleanup().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sh_kills_a_command_that_outlives_its_deadline_and_names_the_escape_hatch() {
        let r = rig();
        let res = sh_concurrent(
            &[ShCommand::Plain(
                "printf 'started\\n'; sleep 60".to_string(),
            )],
            &r.ctx,
            &ShellOptions {
                sh_timeout_ms: Some(200),
                ..r.opts()
            },
        )
        .await;
        assert_eq!(res.len(), 1);
        has(&res[0].out, "killed after 0.2s");
        has(&res[0].out, "bashBg(name, cmd)");
        has(&res[0].out, "started");
        r.cleanup().await;
    }

    // -- the four job verbs ---------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_bg_returns_an_id_and_a_pid_and_publishes_job_spawned() {
        let r = rig();
        let raw = r
            .registry
            .bash_bg("sleeper", "sleep 60", &job_ctx_of(&r.ctx), true)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["id"], "bg_1");
        let pid = v["pid"].as_i64().unwrap();
        assert!(pid > 0, "a live pid must come back to the program");
        let events = r.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].r#type, EventType::JobSpawned);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-1"));
        let job: BackgroundJob = serde_json::from_value(events[0].data.clone()).unwrap();
        assert_eq!(job.id, "bg_1");
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(job.pid, pid);
        r.cleanup().await;
    }

    /// Port note: the TS test wraps the whole run in `script(1)` to guarantee
    /// a controlling terminal and asserts the child cannot open `/dev/tty`.
    /// Here the spawn's own `setsid` is the mechanism under test: the child
    /// has no controlling terminal, so the open fails whether or not the test
    /// harness itself has one.
    #[tokio::test(flavor = "multi_thread")]
    async fn shells_cannot_write_through_the_controlling_terminal() {
        let r = rig();
        let res = sh_concurrent(
            &[ShCommand::Plain(
                "printf LEAK >/dev/tty 2>/dev/null || printf ISOLATED".to_string(),
            )],
            &r.ctx,
            &r.opts(),
        )
        .await;
        // The buffer merges stdout and stderr, so the shell's own redirect
        // diagnostic may ride along — what matters is which branch ran.
        has(&res[0].out, "ISOLATED");
        lacks(
            &res[0].out,
            "LEAK",
            "a shell must never bypass its pipes and repaint the TUI",
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_bg_refuses_a_nameless_job_and_carries_the_name_it_was_given() {
        let r = rig();
        let jctx = job_ctx_of(&r.ctx);
        // Blank, whitespace-only and control-only all fail the same way.
        for bad in ["", "   ", "\n\t"] {
            let err = err_of(r.registry.bash_bg(bad, "sleep 60", &jctx, true));
            assert_eq!(err.name(), "ProgramError");
            has(&err.to_string(), "bashBg needs a NAME");
            has(&err.to_string(), "bashBg(\"dev server\", \"npm run dev\")");
        }
        // A name with no command is the same mistake seen from the other side.
        has(
            &err_of(r.registry.bash_bg("dev server", "", &jctx, true)).to_string(),
            "has no command to run",
        );

        let raw = r
            .registry
            .bash_bg("  dev   server \n", "sleep 60", &jctx, true)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // Normalized on the way in: this string is painted into one rail row.
        assert_eq!(v["name"], "dev server");
        let job: BackgroundJob =
            serde_json::from_value(r.events.lock().unwrap()[0].data.clone()).unwrap();
        assert_eq!(job.name, "dev server");
        has(
            &r.registry.bash_output("bg_1", &r.ctx.session_id).unwrap(),
            "[running]",
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_auto_backgrounded_bash_is_named_from_its_command() {
        let r = rig();
        // The threshold, not a decision, made this a job, so there is no
        // caller to ask for a name — the command's first words are the honest
        // answer.
        let note = bash("NODE_ENV=test sleep 60", &r.ctx, &r.opts_bg(50), "")
            .await
            .unwrap();
        has(&note, "moved to background as bg_1");
        has(&note, "\"sleep 60\"");
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_bg_refuses_past_the_concurrency_cap_and_names_the_running_ids() {
        let r = rig();
        let jctx = job_ctx_of(&r.ctx);
        for _ in 0..8 {
            r.registry
                .bash_bg("sleeper", "sleep 60", &jctx, true)
                .unwrap();
        }
        let err = err_of(r.registry.bash_bg("sleeper", "sleep 60", &jctx, true));
        assert_eq!(err.name(), "ConflictError");
        has(&err.to_string(), "bashKill");
        has(&err.to_string(), "bg_1");
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_output_returns_only_what_accrued_since_the_last_call() {
        let r = rig();
        r.registry
            .bash_bg(
                "two writes",
                "printf 'one\\n'; sleep 1.5; printf 'two\\n'; sleep 60",
                &job_ctx_of(&r.ctx),
                true,
            )
            .unwrap();
        let first = until_accrued(
            "the first chunk",
            || r.registry.bash_output("bg_1", &r.ctx.session_id).unwrap(),
            |s| s.contains("one"),
            10_000,
        )
        .await;
        has(&first, "[running]");
        let second = until_accrued(
            "the second chunk",
            || r.registry.bash_output("bg_1", &r.ctx.session_id).unwrap(),
            |s| s.contains("two"),
            10_000,
        )
        .await;
        lacks(
            &second,
            "one",
            "the cursor must not re-hand output the model already saw",
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_wait_blocks_until_exit_returns_the_exit_line_and_suppresses_the_note() {
        let r = rig();
        r.registry
            .bash_bg(
                "quick failure",
                "printf 'done\\n'; exit 4",
                &job_ctx_of(&r.ctx),
                true,
            )
            .unwrap();
        let out = r
            .registry
            .bash_wait("bg_1", &r.ctx.session_id)
            .await
            .unwrap();
        has(&out, "done");
        has(&out, "[exited with code 4]");
        // Claimed in band — the model already has the result; nothing wakes it.
        assert!(r.notes.lock().unwrap().is_empty());
        assert_eq!(
            r.event_types(),
            vec![EventType::JobSpawned, EventType::JobExited]
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unclaimed_noisy_exit_posts_a_note_a_silent_clean_one_does_not() {
        let r = rig();
        let jctx = job_ctx_of(&r.ctx);
        r.registry
            .bash_bg("oops", "printf 'oops\\n'; exit 2", &jctx, true)
            .unwrap();
        until_true(
            "the completion note",
            || r.notes.lock().unwrap().len() == 1,
            10_000,
        )
        .await;
        {
            let notes = r.notes.lock().unwrap();
            assert_eq!(notes[0].0, r.ctx.session_id);
            has(&notes[0].1, "[background] bg_1 \"oops\" finished (exit 2)");
            has(
                &notes[0].1,
                "1 line of output. Read it with bashOutput(\"bg_1\")",
            );
        }

        // A clean, silent, fire-and-forget exit has nothing to report:
        // notifying would wake an idle session into a whole LLM turn just to
        // say "bg_2 finished". The job.exited event still carries the outcome.
        r.registry
            .bash_bg("silent success", "exit 0", &jctx, true)
            .unwrap();
        until_true(
            "the second job.exited",
            || {
                r.event_types()
                    .iter()
                    .filter(|t| **t == EventType::JobExited)
                    .count()
                    == 2
            },
            10_000,
        )
        .await;
        assert_eq!(r.notes.lock().unwrap().len(), 1);
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_kill_reports_the_real_outcome_and_a_second_kill_reports_the_prior_exit() {
        let r = rig();
        r.registry
            .bash_bg("sleeper", "sleep 60", &job_ctx_of(&r.ctx), true)
            .unwrap();
        let first = r
            .registry
            .bash_kill("bg_1", &r.ctx.session_id)
            .await
            .unwrap();
        assert!(first.starts_with("killed bg_1 ("), "{first}");
        let second = r
            .registry
            .bash_kill("bg_1", &r.ctx.session_id)
            .await
            .unwrap();
        assert!(second.starts_with("bg_1 already exited"), "{second}");
        // A deliberate kill is claimed: it must not also wake the model.
        assert!(r.notes.lock().unwrap().is_empty());
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_job_id_says_what_this_session_actually_has() {
        let r = rig();
        let empty = err_of(r.registry.bash_output("bg_9", &r.ctx.session_id));
        assert_eq!(empty.name(), "NotFoundError");
        has(&empty.to_string(), "has started none");
        has(&empty.to_string(), "bashBg");

        r.registry
            .bash_bg("sleeper", "sleep 60", &job_ctx_of(&r.ctx), true)
            .unwrap();
        let known = err_of(r.registry.bash_output("bg_9", &r.ctx.session_id));
        has(&known.to_string(), "this session has bg_1");
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_cannot_see_or_read_another_sessions_shells() {
        let r = rig_with("sess-a", None);
        r.registry
            .bash_bg("sleeper", "sleep 60", &job_ctx_of(&r.ctx), true)
            .unwrap();
        assert!(r.registry.list_jobs("sess-b").is_empty());
        let err = err_of(r.registry.bash_output("bg_1", "sess-b"));
        assert_eq!(err.name(), "NotFoundError");
        // The jobs API, by contrast, reaches across sessions on purpose:
        // anything the UI can list it must also be able to read and kill.
        assert_eq!(
            r.registry.job_output("bg_1").unwrap().1.session_id,
            "sess-a"
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn job_output_does_not_steal_the_models_bash_output_cursor() {
        let r = rig();
        r.registry
            .bash_bg(
                "shared",
                "printf 'shared\\n'; sleep 60",
                &job_ctx_of(&r.ctx),
                true,
            )
            .unwrap();
        until_true(
            "the UI read",
            || {
                r.registry
                    .job_output("bg_1")
                    .map(|(o, _)| o)
                    .unwrap_or_default()
                    .contains("shared")
            },
            10_000,
        )
        .await;
        // A human looked; the model has still never read this shell.
        has(
            &r.registry.bash_output("bg_1", &r.ctx.session_id).unwrap(),
            "shared",
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn kill_jobs_of_stops_one_sessions_shells_and_kill_all_takes_the_rest() {
        let r = rig_with("sess-a", None);
        let a = job_ctx_of(&r.ctx);
        let b = JobCtx {
            session_id: "sess-b".to_string(),
            workspace: r.ctx.workspace.clone(),
        };
        r.registry.bash_bg("sleeper", "sleep 60", &a, true).unwrap();
        r.registry.bash_bg("sleeper", "sleep 60", &a, true).unwrap();
        r.registry.bash_bg("sleeper", "sleep 60", &b, true).unwrap();
        assert_eq!(r.registry.kill_jobs_of("sess-a"), 2);
        until_true(
            "both exits",
            || r.registry.running_ids("sess-a").is_empty(),
            10_000,
        )
        .await;
        assert_eq!(r.registry.running_ids("sess-b").len(), 1);
        // Server shutdown: a silent shell survives SIGPIPE and must be killed
        // explicitly.
        assert_eq!(r.registry.kill_all(), 1);
        until_true(
            "the last exit",
            || r.registry.running_ids("sess-b").is_empty(),
            10_000,
        )
        .await;
        r.cleanup().await;
    }

    // -- deterministic retention over a live pipe ------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_shells_retained_buffer_keeps_the_head_when_output_overruns_the_budget() {
        let registry = Arc::new(JobRegistry::with_options(JobRegistryOptions {
            limits: TruncateLimits {
                head: Some(40),
                tail: Some(40),
            },
            ..Default::default()
        }));
        let shell = registry
            .spawn(
                MANY_LINES,
                SpawnOpts {
                    cwd: Some(
                        std::env::current_dir()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
        shell.wait_exit().await;
        shell.wait_pumps().await;
        let text = shell_text(&shell);
        // The head is the FIRST bytes the command printed, not the last — a
        // rolling buffer that dropped the oldest would silently rewrite what
        // was already seen.
        assert!(
            text.starts_with("line1\n"),
            "expected the verbatim head, got:\n{text}"
        );
        assert!(
            text.trim_end().ends_with("line200"),
            "expected the verbatim tail, got:\n{text}"
        );
        has(&text, "chars omitted from the middle");
        lacks(&text, "line100", "the middle is what gets omitted");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bash_output_reports_the_hole_when_unread_output_falls_out_of_retention() {
        let registry = Arc::new(JobRegistry::with_options(JobRegistryOptions {
            limits: TruncateLimits {
                head: Some(20),
                tail: Some(20),
            },
            ..Default::default()
        }));
        let ctx = JobCtx {
            session_id: "sess-hole".to_string(),
            workspace: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        };
        registry.bash_bg("noisy", MANY_LINES, &ctx, true).unwrap();
        let seen = registry.bash_wait("bg_1", &ctx.session_id).await.unwrap();
        has(&seen, "chars omitted from the middle");
        has(&seen, "[exited with code 0]");
        registry.drain().await;
    }

    // -- the bridged surface --------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn the_bridged_sh_takes_a_json_array_and_answers_with_json_in_order() {
        let r = rig();
        let host = create_shell_host_fns(r.ctx.clone(), r.opts());
        let out = host
            .sh(&serde_json::json!(["printf 'a\\n'", "exit 5", "printf 'c\\n'"]).to_string())
            .await
            .unwrap();
        let res: Vec<ShResult> = serde_json::from_str(&out).unwrap();
        assert_eq!(
            res,
            vec![
                ShResult {
                    code: 0,
                    out: "a".to_string()
                },
                ShResult {
                    code: 5,
                    out: String::new()
                },
                ShResult {
                    code: 0,
                    out: "c".to_string()
                },
            ]
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_bridged_sh_rejects_a_non_array_payload_with_the_call_it_wanted() {
        let r = rig();
        let host = create_shell_host_fns(r.ctx.clone(), r.opts());
        for bad in ["\"ls\"", "not json at all", "[1,2]"] {
            let err = err_of(host.sh(bad).await);
            assert_eq!(err.name(), "ProgramError");
            has(&err.to_string(), "sh(\"cmd one\", \"cmd two\")");
        }
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_bridged_job_verbs_round_trip_through_the_registry() {
        let r = rig();
        let host = create_shell_host_fns(r.ctx.clone(), r.opts());
        let raw = host
            .bash_bg("bridge check", "printf 'bridged\\n'; exit 0")
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["id"], "bg_1");
        let waited = host.bash_wait("bg_1").await.unwrap();
        has(&waited, "bridged");
        has(&waited, "[exited with code 0]");
        has(&host.bash_output("bg_1").unwrap(), "(no new output)");
        has(&host.bash_kill("bg_1").await.unwrap(), "already exited");
        r.cleanup().await;
    }

    // -- what a command inherits ----------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_command_inherits_bough_session_and_bough_scratch_when_there_is_one() {
        // `$BOUGH_SESSION` is what makes `bough mcp call` enforce the grant
        // belonging to the turn that ran it — the model does not know its own
        // session id, so the value has to arrive in the environment.
        let r = rig_with("sess-42", None);
        let host = create_shell_host_fns(r.ctx.clone(), r.opts());
        let out = host
            .sh(&serde_json::json!(["printf \"%s\" \"$BOUGH_SESSION\""]).to_string())
            .await
            .unwrap();
        let res: Vec<ShResult> = serde_json::from_str(&out).unwrap();
        assert_eq!(
            res,
            vec![ShResult {
                code: 0,
                out: "sess-42".to_string()
            }]
        );

        let dir = temp_dir("bough-scratch");
        let scratch_ctx = ShellCtx {
            scratch: Some(dir.to_string_lossy().into_owned()),
            ..r.ctx.clone()
        };
        let host = create_shell_host_fns(scratch_ctx, r.opts());
        let out = host
            .sh(&serde_json::json!(["printf \"%s\" \"$BOUGH_SCRATCH\""]).to_string())
            .await
            .unwrap();
        let res: Vec<ShResult> = serde_json::from_str(&out).unwrap();
        assert_eq!(
            res,
            vec![ShResult {
                code: 0,
                out: dir.to_string_lossy().into_owned()
            }]
        );
        r.cleanup().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- tags and the history recorder ----------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn the_bridged_bash_requires_tags_and_the_error_teaches_the_format() {
        let r = rig();
        let host = create_shell_host_fns(r.ctx.clone(), r.opts());
        for missing in [None, Some(""), Some("   "), Some(":::")] {
            let err = err_of(host.bash("printf hi", missing).await);
            assert_eq!(err.name(), "ProgramError");
            has(
                &err.to_string(),
                "bash(\"git push origin main\", \"git:push:main\")",
            );
        }
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_finished_bash_records_its_command_normalized_tags_code_and_duration() {
        let r = rig();
        let (ctx, recorded) = recording(&r.ctx);
        let host = create_shell_host_fns(ctx, r.opts());
        host.bash("printf ok", Some(" Git : PUSH ")).await.unwrap();
        host.bash("exit 3", Some("fail:case")).await.unwrap();
        let rows = recorded.lock().unwrap().clone();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].command, "printf ok");
        assert_eq!(rows[0].tags, "git:push");
        assert_eq!(rows[0].exit_code, Some(0));
        assert!(
            rows[0].duration_ms.is_some_and(|d| d >= 0),
            "duration is measured"
        );
        assert_eq!(rows[1].exit_code, Some(3));
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sh_legs_record_untagged_one_row_per_command_codes_intact() {
        let r = rig();
        let (ctx, recorded) = recording(&r.ctx);
        sh_concurrent(
            &[
                ShCommand::Plain("printf a".to_string()),
                ShCommand::Plain("exit 7".to_string()),
            ],
            &ctx,
            &r.opts(),
        )
        .await;
        let rows: Vec<(String, Option<i64>)> = recorded
            .lock()
            .unwrap()
            .iter()
            .map(|e| (e.tags.clone(), e.exit_code))
            .collect();
        assert_eq!(
            rows,
            vec![(String::new(), Some(0)), (String::new(), Some(7))]
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_auto_backgrounded_bash_records_the_real_exit_not_the_handoff() {
        let r = rig();
        let (ctx, recorded) = recording(&r.ctx);
        let out = bash("sleep 0.3; exit 9", &ctx, &r.opts_bg(30), "")
            .await
            .unwrap();
        has(&out, "moved to background");
        assert_eq!(
            recorded.lock().unwrap().len(),
            0,
            "nothing recorded at the handoff — the outcome is not known yet"
        );
        until_true(
            "the backgrounded exit is recorded",
            || recorded.lock().unwrap().len() == 1,
            10_000,
        )
        .await;
        assert_eq!(recorded.lock().unwrap()[0].exit_code, Some(9));
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sh_accepts_cmd_tag_legs_and_records_each_with_its_own_tag() {
        let r = rig();
        let (ctx, recorded) = recording(&r.ctx);
        let host = create_shell_host_fns(ctx, r.opts());
        let out = host
            .sh(&serde_json::json!([
                { "cmd": "printf a", "tag": "Repo: Inspect" },
                { "cmd": "exit 4", "tag": "fail:case" },
                "printf plain",
            ])
            .to_string())
            .await
            .unwrap();
        let res: Vec<ShResult> = serde_json::from_str(&out).unwrap();
        assert_eq!(
            res,
            vec![
                ShResult {
                    code: 0,
                    out: "a".to_string()
                },
                ShResult {
                    code: 4,
                    out: String::new()
                },
                ShResult {
                    code: 0,
                    out: "plain".to_string()
                },
            ]
        );
        let mut rows: Vec<(String, Option<i64>)> = recorded
            .lock()
            .unwrap()
            .iter()
            .map(|e| (e.tags.clone(), e.exit_code))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                (String::new(), Some(0)),
                ("fail:case".to_string(), Some(4)),
                ("repo:inspect".to_string(), Some(0)),
            ]
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sh_shape_error_teaches_the_cmd_tag_form() {
        let r = rig();
        let host = create_shell_host_fns(r.ctx.clone(), r.opts());
        let err = err_of(host.sh("[1,2]").await);
        has(&err.to_string(), "{cmd: \"git push\", tag: \"git:push\"}");
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_recorded_command_carries_its_output_head_and_a_spilled_one_its_file() {
        let dir = temp_dir("bough-spillrec");
        let r = rig();
        let (base, recorded) = recording(&r.ctx);
        let ctx = ShellCtx {
            scratch: Some(dir.to_string_lossy().into_owned()),
            ..base
        };
        let host = create_shell_host_fns(ctx, r.opts());
        host.bash("printf hello-there", Some("smoke:out"))
            .await
            .unwrap();
        // 30k chars: over the spill bound, so the head keeps the marker's path.
        host.bash("yes x | head -c 30000", Some("smoke:spill"))
            .await
            .unwrap();
        let rows = recorded.lock().unwrap().clone();
        assert!(
            rows[0].output_head.starts_with("hello-there"),
            "{}",
            rows[0].output_head
        );
        assert_eq!(rows[0].spill_path, None);
        assert!(
            rows[1]
                .spill_path
                .as_deref()
                .is_some_and(|p| p.starts_with(&*dir.to_string_lossy())),
            "spill path in {:?}",
            rows[1].spill_path
        );
        r.cleanup().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- the memory pushed back ------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_bash_carries_the_memorys_note_below_its_own_output() {
        let r = rig();
        let ctx = ShellCtx {
            echo: Some(EchoHooks {
                note: Arc::new(|_, _, _| Some("[history] seen this before".to_string())),
                guard: Arc::new(|_| None),
            }),
            ..r.ctx.clone()
        };
        let host = create_shell_host_fns(ctx, r.opts());
        let out = host
            .bash("printf boom; exit 1", Some("smoke:echo"))
            .await
            .unwrap();
        has(&out, "boom");
        has(&out, "[history] seen this before");
        assert!(
            out.find("boom").unwrap() < out.find("[history]").unwrap(),
            "the command's own result comes first; the note is a footnote"
        );
        r.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_guarded_command_is_not_spawned_not_recorded_and_says_it_was_skipped() {
        let r = rig();
        let (base, recorded) = recording(&r.ctx);
        let probe = std::env::temp_dir().join(format!("bough-guard-{}", uuid::Uuid::new_v4()));
        let ctx = ShellCtx {
            echo: Some(EchoHooks {
                note: Arc::new(|_, _, _| None),
                guard: Arc::new(|_| Some("[not run] skipped: it keeps failing".to_string())),
            }),
            ..base
        };
        let host = create_shell_host_fns(ctx, r.opts());
        // Would create the file if it ran. It must not run.
        let out = host
            .bash(&format!("touch {}", probe.display()), Some("smoke:guard"))
            .await
            .unwrap();
        assert_eq!(out, "[not run] skipped: it keeps failing");
        assert_eq!(
            recorded.lock().unwrap().len(),
            0,
            "a command that did not run must not enter the memory"
        );
        assert!(!probe.exists());
        r.cleanup().await;
    }
}
