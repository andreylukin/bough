//! Host side of the program worker (port of `src/harness/vm.ts`).
//!
//! The worker stays JavaScript (`js/vm_worker.js`) and runs in a sidecar JS
//! runtime process — Bun if on PATH, else Node — speaking the worker protocol
//! as NDJSON over stdin/stdout (ARCHITECTURE §4.1). **Nothing here is a
//! security boundary** (spec §2.2). THE INVARIANT THIS HOLDS: **a program
//! never outlives its turn, and never takes the server with it.** Three
//! mechanisms:
//!
//!   1. Pre-flight, before the program runs: a `check` message parses the code
//!      in the sidecar engine itself, and a failure comes back with a message
//!      that can explain itself (`preflight::syntax_error_message`).
//!   2. Wind-down is a handshake, not a kill: a program spawns real processes,
//!      so an abort or timeout asks the worker to kill what it spawned, waits
//!      ≤ [`ABORT_GRACE_MS`] for the ack, and only then kills the process
//!      group.
//!   3. Partial output survives: `console.*` lines are streamed as printed
//!      *and* batched; an interrupt kills the worker before it can post its
//!      batch, so the streamed copy kept here is what reaches the model.
//!
//! Concurrency shape (ARCHITECTURE §4.1): one writer task owns stdin (mpsc);
//! the read loop `tokio::select!`s over {stdout line, CancellationToken,
//! wall-clock sleep, grace sleep}; host calls are `tokio::spawn`ed and post
//! results back through the writer — never serially through an awaited call,
//! or a slow `bash` would block `log` lines.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::preflight;
use super::protocol::{FromProgramWorker, HostFnName, ProgramResult};
use crate::types::HostFns;

/// The wall-clock ceiling on one program. Not a resource limit — a liveness
/// one: a program wedged in a synchronous loop or blocked on a host call that
/// will never answer would otherwise hang the turn forever.
pub const DEFAULT_TIMEOUT_MS: u64 = 180_000;

/// How long a stopping program gets to kill the processes it spawned before
/// the worker is terminated regardless. Long enough for a SIGTERM sweep, short
/// enough that the stop button still feels instant.
pub const ABORT_GRACE_MS: u64 = 1_000;

const VM_WORKER_JS: &str = include_str!("js/vm_worker.js");

// ---------------------------------------------------------------------------
// Sidecar runtime + script materialization
// ---------------------------------------------------------------------------

/// The JS runtime the sidecar runs in: `bun` if on PATH, else `node`. Chosen
/// once at first use and logged (ARCHITECTURE §4.1).
pub fn runtime_bin() -> Option<PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        for name in ["bun", "node"] {
            if let Some(p) = find_on_path(name) {
                tracing::info!("code-mode sidecar runtime: {}", p.display());
                return Some(p);
            }
        }
        tracing::warn!("neither bun nor node on PATH — code-mode programs cannot run");
        None
    })
    .clone()
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Materialize an embedded worker script to a cache dir at first use. The
/// filename carries a content hash so a new binary never runs a stale cached
/// copy, and the write is tmp-then-rename so concurrent first uses are safe.
/// `.cjs` because the scripts are CommonJS and Node decides by extension.
///
/// Parameterized by `name`/`src` so the workflow sidecar (`harness/wf.rs`)
/// materializes its own worker through the same path.
pub(crate) fn worker_script_path_for(name: &str, src: &str) -> std::io::Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("bough");
    std::fs::create_dir_all(&dir)?;
    let digest = Sha256::digest(src.as_bytes());
    let hash: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("{name}-{hash}.cjs"));
    if !path.exists() {
        // The temp name is unique per CALL, not per process: two workers (or
        // two tests) materializing at once inside one process shared a
        // pid-named temp file, and the loser's rename hit a path the winner had
        // already moved.
        let tmp = dir.join(format!("{name}-{hash}.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, src)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

fn worker_script_path() -> std::io::Result<PathBuf> {
    worker_script_path_for("vm_worker", VM_WORKER_JS)
}

// ---------------------------------------------------------------------------
// The sidecar process
// ---------------------------------------------------------------------------

/// One spawned worker process. Spawned with `process_group(0)` so the abort
/// path can `killpg` everything the program started that the SIGTERM sweep
/// missed. Stdin is owned by a writer task (mpsc of NDJSON lines); stderr is
/// captured for the `worker error:` path.
pub(crate) struct Sidecar {
    child: Child,
    pid: i32,
    tx: mpsc::UnboundedSender<String>,
    pub(crate) lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
}

impl Sidecar {
    pub(crate) async fn spawn() -> std::io::Result<Sidecar> {
        Sidecar::spawn_script(worker_script_path()?).await
    }

    /// Spawn a sidecar running `script`. The workflow worker
    /// (`harness/wf.rs`) reuses this: same process-group, same writer task,
    /// same stderr capture — one lifecycle, two workers.
    pub(crate) async fn spawn_script(script: PathBuf) -> std::io::Result<Sidecar> {
        let bin = runtime_bin().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "neither bun nor node found on PATH — a code-mode program needs a JS runtime",
            )
        })?;
        let mut child = Command::new(&bin)
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let pid = child.id().unwrap_or(0) as i32;

        // One writer task owns stdin. Dropping the sender closes the pipe,
        // which is also how the worker learns the turn is over.
        let mut stdin = child.stdin.take().expect("stdin piped");
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break; // worker gone — senders will see a closed channel
                }
            }
        });

        let stdout = child.stdout.take().expect("stdout piped");
        let lines = BufReader::new(stdout).lines();

        let stderr_pipe = child.stderr.take().expect("stderr piped");
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = stderr.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr_pipe).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let mut s = sink.lock().expect("stderr sink");
                s.push_str(&l);
                s.push('\n');
            }
        });

        Ok(Sidecar {
            child,
            pid,
            tx,
            lines,
            stderr,
        })
    }

    /// Post one message as an NDJSON line. `false` = the worker is already
    /// gone (posting into a dead worker is not an error worth surfacing).
    pub(crate) fn post(&self, msg: &serde_json::Value) -> bool {
        self.tx.send(msg.to_string()).is_ok()
    }

    /// A sender host-call tasks use to post `host_result`s back through the
    /// writer. Sends after settle land in a closed channel and are dropped.
    pub(crate) fn sender(&self) -> mpsc::UnboundedSender<String> {
        self.tx.clone()
    }

    /// Kill the sidecar process only — normal completion. Children the
    /// program left running keep running, exactly as `worker.terminate()`
    /// left them in TS.
    pub(crate) fn kill(&mut self) {
        let _ = self.child.start_kill();
    }

    /// Kill the whole process group — the abort/timeout backstop after the
    /// handshake. Sweeps whatever a wedged worker could not.
    pub(crate) fn kill_group(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.pid),
            nix::sys::signal::Signal::SIGKILL,
        );
        let _ = self.child.start_kill();
    }

    pub(crate) fn stderr_text(&self) -> String {
        self.stderr.lock().expect("stderr sink").trim().to_string()
    }
}

/// Parse-only round trip for [`preflight::check_program_syntax`]: spawn a
/// sidecar, send `check`, return the engine's raw error message (`None` = the
/// program parses).
pub(crate) async fn engine_check(code: &str) -> std::io::Result<Option<String>> {
    let mut side = Sidecar::spawn().await?;
    side.post(&json!({"type": "check", "code": code}));
    let answer = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match side.lines.next_line().await {
                Ok(Some(l)) => {
                    if let Ok(FromProgramWorker::CheckResult { message, .. }) =
                        serde_json::from_str(&l)
                    {
                        return Ok(message);
                    }
                }
                Ok(None) => {
                    return Err(std::io::Error::other(
                        "sidecar exited during the syntax check",
                    ))
                }
                Err(e) => return Err(e),
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(std::io::Error::other(
            "sidecar did not answer the syntax check",
        ))
    });
    side.kill();
    answer
}

// ---------------------------------------------------------------------------
// Running one program
// ---------------------------------------------------------------------------

/// An options struct on purpose (five positionals grew a bug in the TS
/// pre-history; the caller passes `cancel` and `on_log` but rarely
/// `timeout_ms`).
pub struct RunProgramOptions {
    /// The program source, as the model wrote it.
    pub code: String,
    /// The bridged host functions. **Absence is the capability denial** — a
    /// name the turn does not bridge simply is not here, and calling it
    /// rejects catchably.
    pub host: HostFns,
    /// Wall-clock ceiling. Default [`DEFAULT_TIMEOUT_MS`].
    pub timeout_ms: Option<u64>,
    /// The turn's interrupt. Winds the program down: children first, then the
    /// worker.
    pub cancel: Option<CancellationToken>,
    /// Fires for each `console.*` line as the program prints it. Display-only
    /// — the batched `logs` in the result carry the same lines regardless.
    pub on_log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

/// Spec §6: a timeout and an interrupt must be distinguishable, and each must
/// say what partial work survived. "failed" alone is a defect.
fn survived(lines: usize) -> String {
    if lines == 0 {
        "it printed nothing before stopping; anything it had already done (files written, \
         commands run) still stands"
            .to_string()
    } else {
        format!(
            "the {lines} line(s) it printed before stopping are above; anything it had already \
             done (files written, commands run) still stands"
        )
    }
}

/// Run one program to completion, a timeout, or an interrupt, and resolve with
/// what the model should see. **This never fails**: a program that throws,
/// times out, or is interrupted is an ordinary result with `ok: false`,
/// because every one of those is something the next round can act on.
pub async fn run_program(opts: RunProgramOptions) -> ProgramResult {
    let RunProgramOptions {
        code,
        host,
        timeout_ms,
        cancel,
        on_log,
    } = opts;
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let cancel = cancel.unwrap_or_default();

    let interrupted_msg =
        |lines: usize| format!("program interrupted by the user — {}", survived(lines));
    let timed_out_msg = |lines: usize| {
        format!(
            "program timed out after {timeout_ms}ms — {}. Long-running commands belong in \
             bashBg(name, cmd), not in a foreground wait.",
            survived(lines)
        )
    };

    // Already stopped before we started: the program never ran, so there is
    // nothing to wind down and no reason to wait on an ack.
    if cancel.is_cancelled() {
        return ProgramResult {
            ok: false,
            logs: vec![],
            error: Some(interrupted_msg(0)),
            interrupted: Some(true),
        };
    }

    let mut side = match Sidecar::spawn().await {
        Ok(s) => s,
        Err(e) => {
            return ProgramResult {
                ok: false,
                logs: vec![],
                error: Some(format!("worker error: {e}")),
                interrupted: None,
            }
        }
    };

    // Parse before running. The worker parses it again for real on `run`;
    // this pass exists only to say WHERE, and its engine is the same engine.
    side.post(&json!({"type": "check", "code": code}));

    let host = Arc::new(host);
    // Console lines already streamed out of the worker. An interrupt kills the
    // worker before it can post its batched `logs`, so this copy is what keeps
    // the partial output in the tool result.
    let mut streamed: Vec<String> = Vec::new();
    // In-flight host calls. Dropped (and therefore aborted) when we return —
    // a host call that never answers must not outlive the turn.
    let mut host_tasks: JoinSet<()> = JoinSet::new();

    let wall = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(wall);
    // Armed only when a stop is in flight; parked far in the future until then.
    let grace = tokio::time::sleep(Duration::from_secs(86_400));
    tokio::pin!(grace);
    /// A stop in flight: the message already fixed (line count snapshotted at
    /// stop time, as in TS), waiting on the `aborted` ack or the grace timer.
    struct Winding {
        error: String,
        interrupted: bool,
    }
    let mut winding: Option<Winding> = None;

    // `true` → the wind-down path: kill the whole process group (the backstop
    // for whatever the SIGTERM sweep missed). `false` → normal completion:
    // kill the sidecar only; children the program deliberately left running
    // keep running, exactly as TS `worker.terminate()` left them.
    let (result, group) = loop {
        tokio::select! {
            line = side.lines.next_line() => match line {
                Ok(Some(l)) => {
                    // Stray non-protocol stdout is dropped, not fatal.
                    let Ok(msg) = serde_json::from_str::<FromProgramWorker>(&l) else { continue };
                    match msg {
                        FromProgramWorker::CheckResult { message, .. } => {
                            if let Some(why) = message {
                                // Pre-flight failure: the program never ran —
                                // resolve immediately, logs empty.
                                break (ProgramResult {
                                    ok: false,
                                    logs: vec![],
                                    error: Some(preflight::syntax_error_message(&why, &code)),
                                    interrupted: None,
                                }, false);
                            }
                            side.post(&json!({"type": "run", "code": code}));
                        }
                        FromProgramWorker::Log { line } => {
                            if let Some(cb) = &on_log {
                                cb(&line);
                            }
                            streamed.push(line);
                        }
                        // The worker finished killing what it spawned — stop
                        // waiting on the grace timer.
                        FromProgramWorker::Aborted => {
                            if let Some(w) = winding.take() {
                                break (ProgramResult {
                                    ok: false,
                                    logs: streamed,
                                    error: Some(w.error),
                                    interrupted: w.interrupted.then_some(true),
                                }, true);
                            }
                        }
                        FromProgramWorker::Done { logs } => {
                            break (ProgramResult { ok: true, logs, error: None, interrupted: None }, false);
                        }
                        FromProgramWorker::Error { message, logs } => {
                            break (ProgramResult {
                                ok: false,
                                logs,
                                error: Some(message),
                                interrupted: None,
                            }, false);
                        }
                        FromProgramWorker::Host { id, fn_name, args } => {
                            // Dispatched as a spawned task so a slow host call
                            // never blocks `log` lines (ARCHITECTURE §4.1).
                            let tx = side.sender();
                            let host = host.clone();
                            host_tasks.spawn(async move {
                                let msg = match host_dispatch(&host, &fn_name, args).await {
                                    Ok(v) => json!({"type": "host_result", "id": id, "ok": true, "value": v}),
                                    Err(e) => json!({"type": "host_result", "id": id, "ok": false, "value": e}),
                                };
                                let _ = tx.send(msg.to_string()); // post-settle → dropped
                            });
                        }
                    }
                }
                // Stdout closed. Mid-wind-down that just means the worker died
                // before acking — finish with the pending result. Otherwise the
                // worker crashed: the sidecar equivalent of `worker.onerror`.
                Ok(None) | Err(_) => {
                    if let Some(w) = winding.take() {
                        break (ProgramResult {
                            ok: false,
                            logs: streamed,
                            error: Some(w.error),
                            interrupted: w.interrupted.then_some(true),
                        }, true);
                    }
                    let detail = match side.stderr_text() {
                        s if s.is_empty() => "sidecar exited before posting a result".to_string(),
                        s => s,
                    };
                    break (ProgramResult {
                        ok: false,
                        logs: streamed,
                        error: Some(format!("worker error: {detail}")),
                        interrupted: None,
                    }, true);
                }
            },
            // Stopping is a handshake (idempotent: both stop arms are gated on
            // `winding.is_none()`): ask the worker to kill what it spawned,
            // wait ≤ ABORT_GRACE_MS for the ack, then kill the group. Reverse
            // order orphans processes.
            _ = cancel.cancelled(), if winding.is_none() => {
                let w = Winding { error: interrupted_msg(streamed.len()), interrupted: true };
                if side.post(&json!({"type": "abort"})) {
                    winding = Some(w);
                    grace.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(ABORT_GRACE_MS));
                } else {
                    // Worker already gone — nothing to wind down.
                    break (ProgramResult {
                        ok: false,
                        logs: streamed,
                        error: Some(w.error),
                        interrupted: Some(true),
                    }, true);
                }
            }
            // A timed-out program gets the same wind-down as an interrupted
            // one: whatever it spawned is killed before the worker goes away.
            _ = &mut wall, if winding.is_none() => {
                let w = Winding { error: timed_out_msg(streamed.len()), interrupted: false };
                if side.post(&json!({"type": "abort"})) {
                    winding = Some(w);
                    grace.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(ABORT_GRACE_MS));
                } else {
                    break (ProgramResult {
                        ok: false,
                        logs: streamed,
                        error: Some(w.error),
                        interrupted: None,
                    }, true);
                }
            }
            // A worker wedged in a synchronous loop cannot ack at all, which
            // is exactly why the grace is a timeout and not a wait.
            _ = &mut grace, if winding.is_some() => {
                let w = winding.take().expect("guarded by is_some");
                break (ProgramResult {
                    ok: false,
                    logs: streamed,
                    error: Some(w.error),
                    interrupted: w.interrupted.then_some(true),
                }, true);
            }
        }
    };

    if group {
        side.kill_group();
    } else {
        side.kill();
    }
    result
}

/// One bridged call: validate, run, and hand back what to post. Host failures
/// are catchable program exceptions, never a killed worker.
async fn host_dispatch(
    host: &HostFns,
    fn_name: &str,
    args: Vec<serde_json::Value>,
) -> Result<String, String> {
    // Validate against the canonical list before dispatching: the worker
    // global is program-reachable, so `fn` is not guaranteed to be one of
    // ours.
    let Some(name) = HostFnName::parse(fn_name) else {
        return Err(format!("unknown host function: {fn_name}"));
    };
    let Some(f) = host.get(name) else {
        // Absence is the capability denial. Say which, and that the prompt is
        // the authority — the model must not retry blind.
        return Err(format!(
            "{fn_name}() is not available in this turn — the system prompt lists the host \
             functions this session was granted. Use another approach."
        ));
    };
    // Args are strings by convention; anything else is carried as its JSON.
    let args: Vec<String> = args
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        })
        .collect();
    f(args).await.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests — through a REAL sidecar with trivial programs. Nothing here mocks the
// bridge: the things that can go wrong are ordering and lifecycle, and a fake
// bridge would prove neither (vm.test.ts header; spec §1 quotes it).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::BoughError;
    use crate::harness::protocol::{program_params, HOST_FN_NAMES};
    use crate::types::HostFn;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    // ---- helpers -----------------------------------------------------------

    fn echo(label: &'static str) -> HostFn {
        Arc::new(move |args: Vec<String>| {
            Box::pin(async move { Ok(format!("{label}:{}", args.join("|"))) })
        })
    }

    /// The always-wired half of `HostFns`, each verb echoing what it was
    /// called with so a test can assert the arguments crossed the wire intact.
    fn fake_host() -> HostFns {
        HostFns {
            bash: Some(echo("bash")),
            sh: Some(Arc::new(|args: Vec<String>| {
                Box::pin(async move {
                    let cmds: Vec<serde_json::Value> =
                        serde_json::from_str(args.first().map(String::as_str).unwrap_or("[]"))
                            .unwrap_or_default();
                    let legs: Vec<String> = cmds
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            format!(
                                r#"{{"code":{i},"out":{}}}"#,
                                serde_json::to_string(c.as_str().unwrap_or_default()).unwrap()
                            )
                        })
                        .collect();
                    Ok(format!("[{}]", legs.join(",")))
                })
            })),
            bash_bg: Some(Arc::new(|_args| {
                Box::pin(async { Ok(r#"{"id":"bg_1","pid":4242}"#.to_string()) })
            })),
            bash_output: Some(echo("bashOutput")),
            bash_wait: Some(echo("bashWait")),
            bash_kill: Some(echo("bashKill")),
            view: Some(echo("view")),
            patch: Some(echo("patch")),
            write: Some(echo("write")),
            ..Default::default()
        }
    }

    async fn run(code: &str) -> ProgramResult {
        run_with(code, fake_host()).await
    }

    async fn run_with(code: &str, host: HostFns) -> ProgramResult {
        run_program(RunProgramOptions {
            code: code.into(),
            host,
            timeout_ms: None,
            cancel: None,
            on_log: None,
        })
        .await
    }

    /// The body of the child the wind-down tests spawn: announce, wait, then
    /// claim survival. Run as `<runtime> -e <this>` rather than a shell,
    /// because a program may no longer spawn a shell (the shell doors) and
    /// these tests are about tracking a child process, not about shells.
    fn child_script(pid_file: &Path, marker: &Path) -> String {
        let p = serde_json::to_string(pid_file.to_str().unwrap()).unwrap();
        let m = serde_json::to_string(marker.to_str().unwrap()).unwrap();
        format!(
            "require(\"node:fs\").writeFileSync({p}, String(process.pid));\
             setTimeout(() => require(\"node:fs\").writeFileSync({m}, \"alive\"), 2000);"
        )
    }

    /// The program that spawns that child, runtime-agnostic: `process.execPath`
    /// is bun under a Bun sidecar and node under Node, and spawning a binary
    /// directly is allowed.
    fn spawning_program(script: &str) -> String {
        let s = serde_json::to_string(script).unwrap();
        format!(
            r#"
            const cp = require("node:child_process");
            const child = cp.spawn(process.execPath, ["-e", {s}], {{ stdio: "ignore" }});
            console.log("spawned");
            await new Promise((r) => child.on("exit", r));
            console.log("child exited on its own");
            "#
        )
    }

    /// A file that exists, polled — the child writing it is not synchronous
    /// with our loop.
    async fn wait_for_file(path: &Path, timeout_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdtemp");
        dir
    }

    fn sidecar_is_bun() -> bool {
        runtime_bin()
            .map(|p| p.file_name().is_some_and(|n| n == "bun"))
            .unwrap_or(false)
    }

    // ---- the name lists — the invariant protocol.rs exists to hold ---------

    #[tokio::test]
    async fn worker_binds_exactly_program_params_nothing_missing() {
        // Asked from INSIDE the program: `typeof x` on an undeclared identifier
        // is "undefined" rather than a ReferenceError, so a name the worker
        // forgot to bind shows up as a hole instead of blowing the program up.
        // This probe is what keeps the Rust list and the JS list from drifting
        // (it replaces the TS shared-import invariant).
        let probe = program_params()
            .iter()
            .map(|n| format!("[{n:?}, typeof {n}]"))
            .collect::<Vec<_>>()
            .join(",");
        let res = run(&format!("console.log(JSON.stringify([{probe}]))")).await;

        assert!(res.ok, "{:?}", res.error);
        let seen: Vec<(String, String)> = serde_json::from_str(&res.logs[0]).unwrap();
        let seen: std::collections::HashMap<String, String> = seen.into_iter().collect();

        assert_eq!(seen.len(), program_params().len());
        for name in program_params() {
            let t = seen
                .get(name)
                .unwrap_or_else(|| panic!("{name} is in PROGRAM_PARAMS but absent from the scope"));
            assert_ne!(
                t, "undefined",
                "{name} is declared in protocol.rs but not bound by the worker"
            );
        }
        // Every bridged name is callable — either a function or a
        // verb-dispatched method object (`state.get`, `workflow.start`).
        for name in HOST_FN_NAMES {
            let t = &seen[name];
            assert!(
                t == "function" || t == "object",
                "{name} bound as {t}, which a program cannot call"
            );
        }
        assert_eq!(seen["console"], "object");
    }

    // ---- pre-flight --------------------------------------------------------

    #[tokio::test]
    async fn shadowed_host_name_fails_before_the_program_runs() {
        // The host flags any call — reaching it would mean the program ran.
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let mut host = fake_host();
        host.bash = Some(Arc::new(move |_args| {
            let flag = flag.clone();
            Box::pin(async move {
                flag.store(true, Ordering::SeqCst);
                Ok("must not run".to_string())
            })
        }));
        let res = run_with("let bash = 1;\nawait bash('x')", host).await;

        assert!(!res.ok);
        assert_eq!(res.logs.len(), 0);
        let err = res.error.as_deref().unwrap();
        assert!(err.contains("does not parse"), "{err}");
        // The engine's own words are carried through, whichever engine parsed:
        // JSC says "Cannot declare a let variable twice", V8 says "already
        // been declared".
        assert!(
            err.contains("twice") || err.contains("already been declared"),
            "{err}"
        );
        // Error text is a product surface (spec §6): the message must say WHY
        // the name is taken and what to do, not just quote the parser.
        assert!(err.contains("already bound"), "{err}");
        assert!(err.contains("myBash"), "{err}");
        assert!(
            !called.load(Ordering::SeqCst),
            "the program must never have started"
        );
    }

    #[tokio::test]
    async fn every_host_name_is_reserved_and_clean_code_passes() {
        for name in program_params() {
            let msg = preflight::check_program_syntax(&format!("let {name} = 1;"))
                .await
                .unwrap_or_else(|| panic!("shadowing {name} was not caught"));
            assert!(msg.contains(name), "{msg}");
        }
        assert_eq!(
            preflight::check_program_syntax("const x = await bash('ls');\nconsole.log(x)").await,
            None
        );
        // Shadowing something that is NOT a host name is the program's own
        // business.
        assert_eq!(
            preflight::check_program_syntax("let notAHostFn = 1; let alsoFine = 2;").await,
            None
        );
    }

    #[tokio::test]
    async fn newline_closed_string_names_its_line_and_the_escaping_fix() {
        let msg = preflight::check_program_syntax("const p = \"one\ntwo\";")
            .await
            .expect("a newline-closed string must be caught");
        assert!(msg.contains("line 1"), "{msg}");
        assert!(msg.contains("consumed by the outer literal"), "{msg}");
    }

    // ---- results, logs, host calls -----------------------------------------

    #[tokio::test]
    async fn a_throwing_program_surfaces_its_message() {
        let res =
            run(r#"console.log("before"); throw new Error("boom: the thing exploded");"#).await;

        assert!(!res.ok);
        assert!(
            res.error
                .as_deref()
                .unwrap()
                .contains("boom: the thing exploded"),
            "{:?}",
            res.error
        );
        // Whatever it printed before dying still reaches the model.
        assert_eq!(res.logs[0], "before");
        // Not an interrupt — the turn must be able to tell these apart.
        assert_eq!(res.interrupted, None);
    }

    #[tokio::test]
    async fn a_rejected_host_function_is_an_ordinary_catchable_exception() {
        let mut host = fake_host();
        host.bash = Some(Arc::new(|_args| {
            Box::pin(async { Err(BoughError::program("patch conflict at src/a.ts:74-76")) })
        }));
        let res = run_with(
            r#"try { await bash("x") } catch (e) { console.log("caught " + e.message) }"#,
            host,
        )
        .await;

        assert!(res.ok, "{:?}", res.error);
        assert_eq!(res.logs[0], "caught patch conflict at src/a.ts:74-76");
    }

    #[tokio::test]
    async fn an_unbridged_host_name_rejects_catchably_and_names_the_grant() {
        // `agent` is absent from this host — absence IS the capability denial.
        let res = run(
            r#"try { await agent("do a thing") } catch (e) { console.log("caught " + e.message) }"#,
        )
        .await;

        assert!(res.ok, "{:?}", res.error);
        assert!(
            res.logs[0].contains("agent() is not available in this turn"),
            "{}",
            res.logs[0]
        );
        assert!(res.logs[0].contains("system prompt"), "{}", res.logs[0]);
    }

    #[tokio::test]
    async fn fetch_inside_a_program_is_the_runtimes_not_a_host_verb() {
        // `fetch` was a bridged host function once, which meant the parameter
        // list shadowed the real one. Removing the verb has to leave the
        // ordinary one reachable.
        let res = run(r#"console.log(typeof fetch + " " + (typeof Response));"#).await;

        assert!(res.ok, "{:?}", res.error);
        assert_eq!(res.logs[0], "function function");
    }

    #[tokio::test]
    async fn console_both_streams_live_and_batches_into_the_result() {
        let streamed = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = streamed.clone();
        let res = run_program(RunProgramOptions {
            code: r#"
              console.log("one");
              console.error("two");
              console.warn("three");
              console.info({ a: 1 });
              console.log("multi", "part");
            "#
            .into(),
            host: fake_host(),
            timeout_ms: None,
            cancel: None,
            on_log: Some(Arc::new(move |line| {
                sink.lock().unwrap().push(line.to_string())
            })),
        })
        .await;

        assert!(res.ok, "{:?}", res.error);
        // Same lines, same order, both paths — the stream is display-only and
        // must not change what the model receives (spec §5).
        let expected = vec!["one", "two", "three", r#"{"a":1}"#, "multi part"];
        assert_eq!(res.logs, expected);
        assert_eq!(*streamed.lock().unwrap(), expected);
    }

    #[tokio::test]
    async fn host_calls_round_trip_objects_out_as_json_objects_back_in() {
        let seen = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let mut host = fake_host();
        let s = seen.clone();
        host.bash = Some(Arc::new(move |args: Vec<String>| {
            let s = s.clone();
            Box::pin(async move {
                s.lock().unwrap().push(vec![args[0].clone()]);
                Ok("ok".to_string())
            })
        }));
        let s = seen.clone();
        host.ask = Some(Arc::new(move |args: Vec<String>| {
            let s = s.clone();
            Box::pin(async move {
                s.lock().unwrap().push(args.clone());
                Ok("yes".to_string())
            })
        }));
        let s = seen.clone();
        host.state = Some(Arc::new(move |args: Vec<String>| {
            let s = s.clone();
            Box::pin(async move {
                s.lock().unwrap().push(args.clone());
                Ok(format!(r#"{{"verb":"{}","args":{}}}"#, args[0], args[1]))
            })
        }));
        let res = run_with(
            r#"
            console.log(await bash("echo hi"));
            const shells = await sh("a", "b");
            console.log(JSON.stringify(shells));
            console.log(await ask("ok?", { options: ["y", "n"] }));
            console.log(JSON.stringify(await state.set({ key: "k", value: 1 })));
            "#,
            host,
        )
        .await;

        assert!(res.ok, "{:?}", res.error);
        assert_eq!(res.logs[0], "ok");
        // sh() is variadic program-side, a JSON array on the wire, and returns
        // parsed objects — a non-zero code is data.
        assert_eq!(
            res.logs[1],
            r#"[{"code":0,"out":"a"},{"code":1,"out":"b"}]"#
        );
        assert_eq!(res.logs[2], "yes");
        assert_eq!(
            res.logs[3],
            r#"{"verb":"set","args":{"key":"k","value":1}}"#
        );

        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], vec!["echo hi"]);
        assert_eq!(seen[1], vec!["ok?", r#"{"options":["y","n"]}"#]);
        assert_eq!(seen[2], vec!["set", r#"{"key":"k","value":1}"#]);
    }

    // ---- the exit trap -----------------------------------------------------

    #[tokio::test]
    async fn process_exit_is_catchable_and_does_not_kill_the_worker() {
        let res = run(r#"
          try { process.exit(1) } catch (e) { console.log("caught process.exit: " + e.message) }
          console.log("still running");
        "#)
        .await;

        // The program ran to completion — an untrapped exit would have killed
        // the worker and left the turn hanging until its wall timeout.
        assert!(res.ok, "{:?}", res.error);
        assert!(
            res.logs[0].starts_with("caught process.exit:"),
            "{}",
            res.logs[0]
        );
        assert!(
            res.logs[0].contains("a program ends by returning"),
            "{}",
            res.logs[0]
        );
        assert_eq!(res.logs[1], "still running");
    }

    #[tokio::test]
    async fn an_uncaught_process_exit_surfaces_as_a_program_error_not_a_dead_worker() {
        let res = run(r#"console.log("a"); process.exit(0);"#).await;

        assert!(!res.ok);
        assert!(
            res.error
                .as_deref()
                .unwrap()
                .contains("exit(0) is not available"),
            "{:?}",
            res.error
        );
        assert_eq!(res.logs[0], "a");
    }

    // ---- the shell doors ---------------------------------------------------

    // The assertions are on the ERROR NAMING `bash(cmd, tags)`, because a
    // program that is merely refused and not redirected just tries the next
    // spelling. 9 doors; the 4 Bun-namespace ones exist only under a Bun
    // sidecar (ARCHITECTURE §4.1: under Node they reduce to the child_process
    // patches).
    const SHELL_DOORS: &[(&str, &str)] = &[
        (
            "execSync",
            r#"const { execSync } = await import("node:child_process"); execSync("echo hi");"#,
        ),
        (
            "exec",
            r#"(await import("node:child_process")).exec("echo hi", () => {});"#,
        ),
        (
            "require'd execSync",
            r#"require("child_process").execSync("echo hi");"#,
        ),
        (
            "spawn of sh",
            r#"(await import("node:child_process")).spawn("/bin/sh", ["-c", "echo hi"]);"#,
        ),
        (
            "spawn with shell:true",
            r#"(await import("node:child_process")).spawn("echo", ["hi"], { shell: true });"#,
        ),
    ];
    const BUN_DOORS: &[(&str, &str)] = &[
        ("Bun.spawn of sh", r#"Bun.spawn(["sh", "-c", "echo hi"]);"#),
        (
            "Bun.spawn({cmd}) of bash",
            r#"Bun.spawn({ cmd: ["/bin/bash", "-c", "echo hi"] });"#,
        ),
        (
            "Bun.spawnSync of sh",
            r#"Bun.spawnSync(["sh", "-c", "echo hi"]);"#,
        ),
        ("Bun.$", "Bun.$`echo hi`;"),
    ];

    #[tokio::test]
    async fn a_shell_via_any_door_is_refused_and_points_at_bash_cmd_tags() {
        let redirect = regex::Regex::new(r"bash\(cmd, tags\)|bash\(cmd\)").unwrap();
        let doors: Vec<&(&str, &str)> = if sidecar_is_bun() {
            SHELL_DOORS.iter().chain(BUN_DOORS.iter()).collect()
        } else {
            SHELL_DOORS.iter().collect()
        };
        for (label, code) in doors {
            let res = run(code).await;
            assert!(!res.ok, "{label} was allowed to run a shell");
            assert!(
                redirect.is_match(res.error.as_deref().unwrap_or("")),
                "{label}: {:?}",
                res.error
            );
        }
    }

    #[tokio::test]
    async fn spawning_a_binary_directly_is_still_allowed() {
        // The rule is narrow on purpose: the raw runtime stays open for what
        // the host functions genuinely do not cover. If this test starts
        // failing, the block has grown into a sandbox, which spec §2.2 says it
        // must not be.
        let res = run(r#"
          const cp = require("node:child_process");
          const child = cp.spawn(process.execPath, ["-e", "console.log(1)"], { stdio: "ignore" });
          const code = await new Promise((r) => child.on("exit", r));
          console.log("exit " + code);
        "#)
        .await;

        assert!(res.ok, "{:?}", res.error);
        assert_eq!(res.logs[0], "exit 0");
    }

    // ---- wind-down: children first, then the worker ------------------------

    #[tokio::test]
    async fn an_aborted_program_that_spawned_a_child_leaves_no_orphan() {
        let dir = temp_dir("bough_vm_test_");
        let pid_file = dir.join("pid");
        let marker = dir.join("marker");
        // The child announces itself, waits, then claims it survived. SIGTERM
        // lands on it while it waits, so the marker is never written — an
        // orphan would write it.
        let code = spawning_program(&child_script(&pid_file, &marker));

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(run_program(RunProgramOptions {
            code,
            host: fake_host(),
            timeout_ms: None,
            cancel: Some(cancel.clone()),
            on_log: None,
        }));

        assert!(
            wait_for_file(&pid_file, 10_000).await,
            "the child never started — nothing was tested"
        );
        cancel.cancel();
        let res = handle.await.unwrap();

        assert!(!res.ok);
        // Interrupt and timeout must be distinguishable, and the message must
        // say what survived (spec §6).
        assert_eq!(res.interrupted, Some(true));
        let err = res.error.as_deref().unwrap();
        assert!(err.contains("interrupted by the user"), "{err}");
        assert!(err.contains("still stands"), "{err}");
        // Streamed output survives the kill that beat the worker's batch.
        assert_eq!(res.logs[0], "spawned");
        assert!(
            !res.logs.iter().any(|l| l == "child exited on its own"),
            "the program should not have finished"
        );

        // The load-bearing assertion: if the sweep ran in the wrong order — or
        // not at all — the child's timer completes and the marker appears.
        tokio::time::sleep(Duration::from_millis(3_000)).await;
        assert!(
            !marker.exists(),
            "the child outlived the abort — orphaned process"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_abort_handshake_sweeps_children_before_it_acks() {
        // The end-to-end test above asserts the OUTCOME (no orphan), which is
        // the thing that matters — but the group-kill backstop would also pass
        // it with the sweep removed. This test drives the worker protocol by
        // hand and never kills the group before checking, so the worker (and
        // its process group) is still alive when the marker would be written.
        // Nothing but `killChildren()` can have stopped the child.
        let dir = temp_dir("bough_vm_sweep_");
        let pid_file = dir.join("pid");
        let marker = dir.join("marker");
        let program = spawning_program(&child_script(&pid_file, &marker));

        let mut side = Sidecar::spawn().await.expect("sidecar");
        side.post(&json!({"type": "run", "code": program}));

        assert!(
            wait_for_file(&pid_file, 10_000).await,
            "the child never started — nothing was tested"
        );
        side.post(&json!({"type": "abort"}));
        // The ack is the worker's promise that the sweep already ran — the
        // host may terminate only after it.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match side.lines.next_line().await {
                    Ok(Some(l)) => {
                        if matches!(
                            serde_json::from_str::<FromProgramWorker>(&l),
                            Ok(FromProgramWorker::Aborted)
                        ) {
                            break;
                        }
                    }
                    _ => panic!("sidecar died before acking the abort"),
                }
            }
        })
        .await
        .expect("no aborted ack within 10s");

        tokio::time::sleep(Duration::from_millis(3_000)).await;
        assert!(
            !marker.exists(),
            "the abort acked without killing the child"
        );

        side.kill_group();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_wall_clock_timeout_is_reported_as_a_timeout_not_an_interrupt() {
        let res = run_program(RunProgramOptions {
            code: r#"console.log("started"); await new Promise((r) => setTimeout(r, 30_000));"#
                .into(),
            host: fake_host(),
            timeout_ms: Some(300),
            cancel: None,
            on_log: None,
        })
        .await;

        assert!(!res.ok);
        let err = res.error.as_deref().unwrap();
        assert!(err.contains("timed out after 300ms"), "{err}");
        // The two stop reasons must not be confusable — the turn persists one
        // of them.
        assert!(!err.contains("interrupted"), "{err}");
        assert_eq!(res.interrupted, None);
        assert_eq!(res.logs[0], "started");
        // Says what to do instead of a foreground wait (spec §6).
        assert!(err.contains("bashBg"), "{err}");
    }

    #[tokio::test]
    async fn a_signal_already_aborted_never_starts_the_program() {
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let mut host = fake_host();
        host.bash = Some(Arc::new(move |_args| {
            let flag = flag.clone();
            Box::pin(async move {
                flag.store(true, Ordering::SeqCst);
                Ok("must not run".to_string())
            })
        }));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let res = run_program(RunProgramOptions {
            code: r#"await bash("echo hi")"#.into(),
            host,
            timeout_ms: None,
            cancel: Some(cancel),
            on_log: None,
        })
        .await;

        assert!(!res.ok);
        assert_eq!(res.interrupted, Some(true));
        assert_eq!(res.logs.len(), 0);
        assert!(
            !called.load(Ordering::SeqCst),
            "the program must never have started"
        );
    }

    #[tokio::test]
    async fn an_interrupt_mid_host_call_still_winds_down_and_keeps_partial_output() {
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let mut host = fake_host();
        // A host function that never answers — the real ones die on the turn's
        // own signal, but the bridge must not depend on that to stop.
        host.bash = Some(Arc::new(move |_args| {
            let flag = flag.clone();
            Box::pin(async move {
                flag.store(true, Ordering::SeqCst);
                futures::future::pending::<Result<String, BoughError>>().await
            })
        }));

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(run_program(RunProgramOptions {
            code: r#"console.log("about to hang"); await bash("sleep 999"); console.log("unreachable");"#
                .into(),
            host,
            timeout_ms: None,
            cancel: Some(cancel.clone()),
            on_log: None,
        }));
        while !called.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
        let res = handle.await.unwrap();

        assert!(!res.ok);
        assert_eq!(res.interrupted, Some(true));
        assert_eq!(res.logs[0], "about to hang");
        assert!(!res.logs.iter().any(|l| l == "unreachable"));
    }

    /// `require` is bound because weak models write CommonJS by reflex, and
    /// spec §2.2 already grants the program the very modules it reaches for.
    #[tokio::test]
    async fn a_program_may_reach_node_builtins_through_require_not_only_import() {
        let res = run(r#"
          const path = require("node:path");
          console.log(path.join("a", "b"));
        "#)
        .await;

        assert!(res.ok, "{:?}", res.error);
        assert_eq!(res.logs, vec!["a/b"]);
    }
}
