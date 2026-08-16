//! Background shells and the retained-output registry every shell surface
//! reads from. Port of `src/hostfn/jobs.ts` (spec: hostfn.md §jobs).
//!
//! A shell lands here two ways: **explicitly** via `bashBg` — fire-and-forget
//! work that is *supposed* to outlive the turn, spawned WITHOUT the turn's
//! interrupt — and **automatically**, when a foreground `bash` is still
//! running at the background threshold and the running child is `promote`d
//! here instead of being killed.
//!
//! THE INVARIANT THIS HOLDS: **a long command is never lost and never blocks
//! the turn.**
//!
//!   1. Buffers are retained per shell and readable **while running**.
//!   2. Retention is bounded but **deterministic**: head and tail kept
//!      verbatim with an explicit omission marker in between. No LLM
//!      digestion — a bounded buffer that dropped the *head* would silently
//!      rewrite what the model already saw.
//!   3. Exit is **announced**, not discovered: `job.spawned`/`job.exited` bus
//!      events, and an unclaimed exit posts a `[background]` system note.
//!
//! Shells are registered per session, in memory: they persist across rounds
//! and turns and die with the server process. That is why `BackgroundJob` is
//! not a table — a persisted row would always be a lie after a restart.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use nix::sys::signal::Signal;
use nix::unistd::Pid;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use crate::bus::Bus;
use crate::errors::BoughError;
use crate::hostfn::spill::{
    omission_marker, spill, RealSpillDeps, SpillCtx, SpillDeps, SpillSink, TruncateLimits,
    MAX_HEAD_CHARS, MAX_TAIL_CHARS, SPILL_OVER_CHARS,
};
use crate::schema::events::{EventInput, EventType};
use crate::schema::parts::{BackgroundJob, JobStatus};
use crate::types::{system_clock, Clock};

// ---------------------------------------------------------------------------
// One shell
// ---------------------------------------------------------------------------

/// Grace between SIGTERM and the SIGKILL backstop in `bash_kill`.
const KILL_GRACE_MS: u64 = 2_000;
/// Running `bashBg` shells per session — a brake on loops that spawn and
/// forget.
const MAX_RUNNING: usize = 8;
/// How long an exited shell stays in `list_jobs`. Long enough that the
/// outcome of a job you started is still there when you look up.
const RECENT_MS: i64 = 30 * 60_000;
/// Longest job name kept. Past this it is a description, not a label.
const MAX_NAME_CHARS: usize = 60;

/// How a shell ended. `code` is 0 for a signal death (parity with
/// `child.exitCode ?? 0`); the signal carries the distinction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: i64,
    pub signal: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct ResolvedLimits {
    head: usize,
    tail: usize,
}

/// The mutable half of a tracked shell, under one short-lived mutex. The
/// buffer is three fields rather than one string because retention is
/// head + tail: `head` fills once and is then immutable, `tail` rolls, and
/// `written` counts everything the process ever produced.
struct ShellState {
    /// Assigned by `promote`; `""` while a foreground `bash` still owns it.
    id: String,
    /// What this job IS, in the words of whoever started it. Assigned by
    /// `promote` alongside the id.
    name: String,
    session_id: String,
    started_at: i64,
    ended_at: Option<i64>,
    /// Set by `bash_kill`/`kill_jobs_of` so the exit reads as a kill.
    killed: bool,
    /// First `limits.head` chars, verbatim and immutable once full.
    head: String,
    /// Rolling last `limits.tail` chars.
    tail: String,
    /// Total chars the process has produced, including what retention
    /// dropped.
    written: usize,
    /// The complete on-disk copy, once output grew large enough to earn one.
    sink: Option<SpillSink>,
    /// Chars of the stream already handed to `bash_output`.
    read_to: usize,
    status: Option<ExitStatus>,
    /// `bash_wait`/`bash_kill` set this: the result was taken in band,
    /// suppress the note.
    claimed: bool,
    /// Guards against a double completion note.
    notified: bool,
}

/// A tracked shell. Shared as `Arc<Shell>` between the pump tasks, the exit
/// task and every reader.
pub struct Shell {
    pub command: String,
    pub pid: i32,
    /// The session scratchpad, when this shell belongs to a session. Carried
    /// on the shell because the places output leaves this module are called
    /// from contexts that no longer hold the spawn options.
    pub scratch: Option<String>,
    limits: ResolvedLimits,
    state: Mutex<ShellState>,
    exit_rx: watch::Receiver<Option<ExitStatus>>,
    pumps_rx: watch::Receiver<bool>,
    /// The once-guard on `handle_on_exit` (the TS `onExit` fires exactly once
    /// because the exit promise resolves once; here concurrency needs a flag).
    exit_handled: AtomicBool,
}

impl Shell {
    pub fn status(&self) -> Option<ExitStatus> {
        self.state.lock().unwrap().status.clone()
    }

    pub fn id(&self) -> String {
        self.state.lock().unwrap().id.clone()
    }

    pub fn name(&self) -> String {
        self.state.lock().unwrap().name.clone()
    }

    /// Resolves with the exit status — the analog of the TS `exit` promise.
    pub async fn wait_exit(&self) -> ExitStatus {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(s) = rx.borrow().clone() {
                return s;
            }
            if rx.changed().await.is_err() {
                // Sender dropped; read whatever landed.
                return rx.borrow().clone().unwrap_or(ExitStatus {
                    code: 0,
                    signal: None,
                });
            }
        }
    }

    /// Resolves when both stdout and stderr have fully drained.
    pub async fn wait_pumps(&self) {
        let mut rx = self.pumps_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

/// A job name, normalized — or `None` when there is nothing usable in it.
///
/// Whitespace collapses and control characters go, because this string is
/// painted into a single rail row and an embedded escape sequence would
/// repaint the screen.
pub fn normalize_job_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if (c as u32) < 0x20 || c as u32 == 0x7f {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if char_len(&collapsed) > MAX_NAME_CHARS {
        Some(format!("{}…", take_chars(&collapsed, MAX_NAME_CHARS - 1)))
    } else {
        Some(collapsed)
    }
}

/// A name for a shell nobody named: the auto-background path. The command's
/// first meaningful words, with a `cd … &&` prelude and leading `VAR=value`
/// assignments dropped, because `NODE_ENV=test npm test` is a test run and
/// not an environment variable.
pub fn derive_name(command: &str) -> String {
    use std::sync::OnceLock;
    static CD: OnceLock<regex::Regex> = OnceLock::new();
    static VARS: OnceLock<regex::Regex> = OnceLock::new();
    static SPLIT: OnceLock<regex::Regex> = OnceLock::new();
    let cd = CD.get_or_init(|| regex::Regex::new(r"^(?:cd\s+\S+\s*&&\s*)+").unwrap());
    let vars = VARS.get_or_init(|| {
        regex::Regex::new(r#"^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S*)\s+)+"#).unwrap()
    });
    let split = SPLIT.get_or_init(|| regex::Regex::new(r"\s*(?:\||&&|;)\s*").unwrap());

    let first_line = command.trim().split('\n').next().unwrap_or("");
    let rest = cd.replace(first_line, "");
    let rest = vars.replace(&rest, "");
    let head = split.split(&rest).next().unwrap_or("");
    normalize_job_name(head).unwrap_or_else(|| "shell".to_string())
}

/// Append to the retained buffer, maintaining head-then-rolling-tail, and
/// stream to the sink.
fn append(shell: &Shell, text: &str) {
    let mut st = shell.state.lock().unwrap();
    st.written += char_len(text);
    retain(&mut st, shell.limits, text);
    // AFTER the buffer update, so that when the sink opens mid-stream the
    // pending buffer includes the chunk that crossed the threshold. Opening
    // first and writing only the prior buffer silently dropped exactly one
    // chunk — 262,144 characters of a 1.29MB command — from a file whose
    // banner said it held everything.
    let pending = if st.sink.is_none() && st.written > SPILL_OVER_CHARS {
        // Everything so far. Whole at this moment, because the spill
        // threshold is an order of magnitude below the retention cap.
        format!("{}{}", st.head, st.tail)
    } else {
        String::new()
    };
    let ctx = SpillCtx {
        scratch: shell.scratch.clone(),
        budget: None,
        label: Some(if st.id.is_empty() {
            "bash".to_string()
        } else {
            st.id.clone()
        }),
    };
    let total = st.written;
    st.sink = crate::hostfn::spill::stream_spill(
        st.sink.take(),
        text,
        &ctx,
        total,
        move || pending,
        &RealSpillDeps,
    );
}

/// The head-then-rolling-tail buffer, on its own so `append` has no early
/// exit.
fn retain(st: &mut ShellState, limits: ResolvedLimits, text: &str) {
    let head_len = char_len(&st.head);
    let mut rest = text;
    if head_len < limits.head {
        let take = (limits.head - head_len).min(char_len(rest));
        st.head.push_str(take_chars(rest, take));
        rest = skip_chars(rest, take);
    }
    if rest.is_empty() {
        return;
    }
    st.tail.push_str(rest);
    let tail_len = char_len(&st.tail);
    if tail_len > limits.tail {
        st.tail = skip_chars(&st.tail, tail_len - limits.tail).to_string();
    }
}

/// The retained stream from absolute offset `from`, with the marker standing
/// in for whatever retention dropped.
fn retained_from(st: &ShellState, from: usize) -> (String, usize) {
    let head_end = char_len(&st.head);
    let tail_len = char_len(&st.tail);
    let tail_start = st.written - tail_len;
    let mut parts: Vec<String> = Vec::new();
    if from < head_end {
        parts.push(skip_chars(&st.head, from).to_string());
    }
    let gap_from = from.max(head_end);
    let omitted = tail_start.saturating_sub(gap_from);
    if omitted > 0 {
        parts.push(omission_marker(omitted, st.written));
    }
    let tail_from = from.max(tail_start);
    if tail_from < st.written {
        parts.push(skip_chars(&st.tail, tail_from - tail_start).to_string());
    }
    (parts.concat(), omitted)
}

/// Everything retained, from the beginning.
pub fn shell_text(shell: &Shell) -> String {
    retained_from(&shell.state.lock().unwrap(), 0).0
}

/// The inline format for a finished foreground command (see `shell.rs`).
pub fn format_final(shell: &Shell, deps: &dyn SpillDeps) -> String {
    let (text, ctx, sink, status) = {
        let st = shell.state.lock().unwrap();
        (
            retained_from(&st, 0).0,
            SpillCtx {
                scratch: shell.scratch.clone(),
                budget: None,
                label: Some("bash".to_string()),
            },
            st.sink.clone(),
            st.status.clone(),
        )
    };
    // Spilled before the exit line is appended, so the marker cannot be
    // separated from the output it describes.
    let body = spill(&text, &ctx, sink.as_ref(), deps)
        .trim_end()
        .to_string();
    let mut parts: Vec<String> = Vec::new();
    if !body.is_empty() {
        parts.push(body);
    }
    let code = status.as_ref().map(|s| s.code).unwrap_or(0);
    let signal = status.as_ref().and_then(|s| s.signal.clone());
    if code != 0 || signal.is_some() {
        parts.push(format!(
            "[exit code {code}{}]",
            signal.map(|s| format!(" on {s}")).unwrap_or_default()
        ));
    }
    let joined = parts.join("\n");
    if joined.is_empty() {
        "(no output)".to_string()
    } else {
        joined
    }
}

/// What a foreground `bash` returns once it auto-backgrounds.
///
/// Every clause is load-bearing: the command is still ALIVE (so the model
/// must not re-run it), the id is how to reach it, and the three verbs are
/// named outright so the next round reads progress instead of inventing a
/// sleep loop.
pub fn background_note(shell: &Shell, id: &str, after_ms: u64) -> String {
    let (text, name, ctx, sink) = {
        let mut st = shell.state.lock().unwrap();
        let (text, _) = retained_from(&st, st.read_to);
        st.read_to = st.written;
        (
            text,
            st.name.clone(),
            SpillCtx {
                scratch: shell.scratch.clone(),
                budget: None,
                label: Some(id.to_string()),
            },
            st.sink.clone(),
        )
    };
    let secs = (after_ms as f64 / 1000.0).round() as i64;
    let head = format!(
        "[still running after {secs}s — moved to background as \
         {id}{}. It keeps running; you'll be notified \
         when it finishes. Read progress: \
         bashOutput(\"{id}\"); block until done: bashWait(\"{id}\"); stop it: bashKill(\"{id}\").]",
        if name.is_empty() {
            String::new()
        } else {
            format!(" \"{name}\"")
        },
    );
    let so_far = spill(&text, &ctx, sink.as_ref(), &RealSpillDeps)
        .trim_end()
        .to_string();
    if so_far.is_empty() {
        head
    } else {
        format!("{head}\n{so_far}")
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// What a job verb needs to know about its caller. A `TurnCtx` satisfies it.
#[derive(Clone, Debug, Default)]
pub struct JobCtx {
    pub session_id: String,
    pub workspace: String,
}

/// Posts the `[background] bg_N finished …` system note. The seam is here —
/// the turn runner hands the registry a notifier at wiring time.
pub type JobNotifier = Arc<dyn Fn(&str, &str) + Send + Sync>;

#[derive(Default)]
pub struct JobRegistryOptions {
    /// Where `job.spawned`/`job.exited` go. Absent = no events (unit tests).
    pub bus: Option<Arc<Bus>>,
    pub notify: Option<JobNotifier>,
    /// Injected clock. Absent = the system clock.
    pub now: Option<Clock>,
    /// Retention budget per shell.
    pub limits: TruncateLimits,
    /// Concurrent `bashBg` shells allowed per session.
    pub max_running: Option<usize>,
}

/// Options for `spawn`. The turn's abort signal is deliberately NOT here —
/// the tree walk must snapshot descendants before the direct child dies, so
/// interrupts attach a listener instead (`shell.rs::kill_tree_on_abort`).
#[derive(Clone, Debug, Default)]
pub struct SpawnOpts {
    pub cwd: Option<String>,
    pub scratch: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PromoteOpts {
    pub force: bool,
    pub name: Option<String>,
}

/// The last `lines` lines of a shell's buffer, plus how many there are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobTail {
    pub tail: Vec<String>,
    pub output_lines: usize,
}

struct Inner {
    /// sessionId → shells, in registration order.
    sessions: HashMap<String, Vec<Arc<Shell>>>,
    /// Foreground shells currently inside `bash`, by session.
    foreground: HashMap<String, Vec<Arc<Shell>>>,
    seq: u64,
}

/// The per-session shell registry.
///
/// A struct rather than module state so a test can construct one, drive it,
/// and throw it away — the server builds exactly ONE and hangs it on
/// `HostState`.
pub struct JobRegistry {
    inner: Mutex<Inner>,
    bus: Mutex<Option<Arc<Bus>>>,
    notify: Mutex<Option<JobNotifier>>,
    now: Clock,
    limits: ResolvedLimits,
    max_running: usize,
    /// Serializes "decide + emit" so `job.exited` can never overtake
    /// `job.spawned` for one shell (the TS single thread gave this free).
    events_lock: Mutex<()>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::with_options(JobRegistryOptions::default())
    }

    pub fn with_options(options: JobRegistryOptions) -> Self {
        JobRegistry {
            inner: Mutex::new(Inner {
                sessions: HashMap::new(),
                foreground: HashMap::new(),
                seq: 0,
            }),
            bus: Mutex::new(options.bus),
            notify: Mutex::new(options.notify),
            now: options.now.unwrap_or_else(system_clock),
            limits: ResolvedLimits {
                head: options.limits.head.unwrap_or(MAX_HEAD_CHARS),
                tail: options.limits.tail.unwrap_or(MAX_TAIL_CHARS),
            },
            max_running: options.max_running.unwrap_or(MAX_RUNNING),
            events_lock: Mutex::new(()),
        }
    }

    /// Wire the bus after construction — the server builds one before the
    /// other.
    pub fn attach_bus(&self, bus: Arc<Bus>) {
        *self.bus.lock().unwrap() = Some(bus);
    }

    /// Wire the `[background]` system-note poster after construction.
    pub fn attach_notifier(&self, notify: JobNotifier) {
        *self.notify.lock().unwrap() = Some(notify);
    }

    // -- spawning -------------------------------------------------------------

    /// Spawn a shell and start pumping its output. Does NOT register it: a
    /// foreground bash uses this to stream while it decides whether to
    /// background or return inline.
    ///
    /// The child gets its own session (`setsid`): ignored stdin alone is
    /// insufficient — an interactive program can open /dev/tty directly and
    /// paint a password prompt over the TUI's alternate screen. A fresh
    /// session makes that open fail while stdout/stderr still flow through
    /// our pipes.
    pub fn spawn(self: &Arc<Self>, command: &str, opts: SpawnOpts) -> std::io::Result<Arc<Shell>> {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        if let Some(cwd) = &opts.cwd {
            cmd.current_dir(cwd);
        }
        // `$BOUGH_SCRATCH` in the shell, because the prompt's sentence about
        // a scratchpad reaches the model and not the command it writes.
        if let Some(s) = &opts.scratch {
            cmd.env("BOUGH_SCRATCH", s);
        }
        // `$BOUGH_SESSION` for the same reason, one level up: `bough mcp`
        // scopes grants by it, and the model must never compose it.
        if let Some(sid) = &opts.session_id {
            cmd.env("BOUGH_SESSION", sid);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // ssh/sudo/pinentry can bypass stdio through /dev/tty. A fresh
        // session cannot acquire the TUI's controlling terminal.
        unsafe {
            cmd.pre_exec(|| {
                let _ = nix::unistd::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0) as i32;

        let (exit_tx, exit_rx) = watch::channel::<Option<ExitStatus>>(None);
        let (pumps_tx, pumps_rx) = watch::channel(false);
        let shell = Arc::new(Shell {
            command: command.to_string(),
            pid,
            scratch: opts.scratch.clone(),
            limits: self.limits,
            state: Mutex::new(ShellState {
                id: String::new(),
                name: String::new(),
                session_id: String::new(),
                started_at: (self.now)(),
                ended_at: None,
                killed: false,
                head: String::new(),
                tail: String::new(),
                written: 0,
                sink: None,
                read_to: 0,
                status: None,
                claimed: false,
                notified: false,
            }),
            exit_rx,
            pumps_rx,
            exit_handled: AtomicBool::new(false),
        });

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let p1 = tokio::spawn(pump(stdout, shell.clone()));
        let p2 = tokio::spawn(pump(stderr, shell.clone()));
        tokio::spawn(async move {
            let _ = p1.await;
            let _ = p2.await;
            let _ = pumps_tx.send(true);
        });

        let sh = shell.clone();
        let now = self.now.clone();
        let registry: Weak<JobRegistry> = Arc::downgrade(self);
        tokio::spawn(async move {
            let status = child.wait().await;
            let es = match status {
                Ok(st) => exit_status_of(st),
                Err(_) => ExitStatus {
                    code: 0,
                    signal: None,
                },
            };
            let promoted = {
                let mut st = sh.state.lock().unwrap();
                st.status = Some(es.clone());
                st.ended_at = Some(now());
                !st.id.is_empty()
            };
            // Let the pipes drain (bounded) before anything is announced, so
            // the completion note counts the lines the command actually
            // printed. In TS the event loop delivered the final pipe chunks
            // before the exit promise resolved; tokio's `wait()` can win that
            // race. Bounded, because a grandchild that inherited the pipes (a
            // backgrounded dev server) must not stall the exit announcement.
            let _ = tokio::time::timeout(Duration::from_millis(1_000), sh.wait_pumps()).await;
            // Announce BEFORE waking waiters, so a caller resuming from
            // `wait_exit` observes the events already published.
            if promoted {
                if let Some(reg) = registry.upgrade() {
                    reg.handle_on_exit(&sh);
                }
            }
            let _ = exit_tx.send(Some(es));
        });

        Ok(shell)
    }

    /// Register a running shell so later rounds and turns can read it, and
    /// wire its completion note. Returns the assigned id, or `None` when the
    /// session is already at the concurrency cap.
    ///
    /// Auto-background never kills, and the cap exists to brake `bashBg`
    /// loops the model chose to write — so the auto-background path forces
    /// registration and the cap stays on explicit `bashBg`.
    pub fn promote(
        self: &Arc<Self>,
        shell: &Arc<Shell>,
        ctx: &JobCtx,
        opts: PromoteOpts,
    ) -> Option<String> {
        let id = {
            let mut inner = self.inner.lock().unwrap();
            if !opts.force && running_count(&inner, &ctx.session_id) >= self.max_running {
                return None;
            }
            inner.seq += 1;
            let id = format!("bg_{}", inner.seq);
            {
                let mut st = shell.state.lock().unwrap();
                st.id = id.clone();
                // Never empty. `bashBg` has already refused a blank one; the
                // auto-background path passes none and gets the command's own
                // first words.
                st.name = opts
                    .name
                    .as_deref()
                    .and_then(normalize_job_name)
                    .unwrap_or_else(|| derive_name(&shell.command));
                st.session_id = ctx.session_id.clone();
            }
            inner
                .sessions
                .entry(ctx.session_id.clone())
                .or_default()
                .push(shell.clone());
            id
        };
        // Raced a near-instant exit between the caller's threshold and here.
        let already_exited = shell.status().is_some();
        if already_exited {
            self.handle_on_exit(shell);
        } else {
            let _g = self.events_lock.lock().unwrap();
            if !shell.exit_handled.load(Ordering::SeqCst) {
                self.emit(EventType::JobSpawned, shell);
            }
        }
        Some(id)
    }

    // -- the four job verbs ---------------------------------------------------

    /// Spawn `command` detached under `name`; returns `{id, name, pid}` as
    /// JSON. The name is REQUIRED and refused when blank rather than derived:
    /// a background job is the one thing here the user watches without having
    /// read the round that started it. `wake: false` suppresses the
    /// completion note (the TUI `!cmd` path) and is set BEFORE the process
    /// can exit.
    pub fn bash_bg(
        self: &Arc<Self>,
        name: &str,
        command: &str,
        ctx: &JobCtx,
        wake: bool,
    ) -> Result<String, BoughError> {
        let label = normalize_job_name(name);
        let Some(label) = label else {
            return Err(BoughError::program(
                "bashBg needs a NAME for the job before the command: \
                 bashBg(\"dev server\", \"npm run dev\"). The name is what the user sees in \
                 the live-work rail and in the finished-job note, so make it say what the \
                 job is for, not what the command is."
                    .to_string(),
            ));
        };
        if command.trim().is_empty() {
            return Err(BoughError::program(format!(
                "bashBg(\"{label}\", …) has no command to run — the name comes first now: \
                 bashBg(name, cmd).",
            )));
        }
        let running = {
            let inner = self.inner.lock().unwrap();
            running_count(&inner, &ctx.session_id)
        };
        if running >= self.max_running {
            return Err(BoughError::conflict(format!(
                "this session already has {running} running background shells (the cap is \
                 {}) — bashKill one of {} first, or wait for one to finish with bashWait.",
                self.max_running,
                self.running_ids(&ctx.session_id).join(", "),
            )));
        }
        // No signal: an explicit background shell survives the turn's stop
        // button.
        let shell = self
            .spawn(
                command,
                SpawnOpts {
                    cwd: Some(ctx.workspace.clone()),
                    ..Default::default()
                },
            )
            .map_err(|e| BoughError::program(format!("could not start command: {e}")))?;
        // Set BEFORE promote wires the exit path: `notified` is the flag the
        // exit handler checks, and a fast command can finish immediately.
        if !wake {
            shell.state.lock().unwrap().notified = true;
        }
        let id = self
            .promote(
                &shell,
                ctx,
                PromoteOpts {
                    force: true,
                    name: Some(label),
                },
            )
            .expect("forced promote always succeeds");
        let name = shell.name();
        Ok(serde_json::json!({ "id": id, "name": name, "pid": shell.pid }).to_string())
    }

    /// Output accrued since the last `bash_output(id)` call, plus a status
    /// line. Safe while the shell is still running — this is how a program
    /// watches progress without polling the process itself.
    pub fn bash_output(&self, id: &str, session_id: &str) -> Result<String, BoughError> {
        let shell = self.require(id, session_id)?;
        let (text, ctx, sink, status) = {
            let mut st = shell.state.lock().unwrap();
            let (text, _) = retained_from(&st, st.read_to);
            st.read_to = st.written;
            (
                text,
                SpillCtx {
                    scratch: shell.scratch.clone(),
                    budget: None,
                    label: Some(if id.is_empty() {
                        "bg".to_string()
                    } else {
                        id.to_string()
                    }),
                },
                st.sink.clone(),
                st.status.clone(),
            )
        };
        let fresh = spill(&text, &ctx, sink.as_ref(), &RealSpillDeps)
            .trim_end()
            .to_string();
        let status_line = match status {
            None => "[running]".to_string(),
            Some(s) => format!(
                "[exited with code {}{}]",
                s.code,
                s.signal.map(|sig| format!(" on {sig}")).unwrap_or_default()
            ),
        };
        let body = if fresh.is_empty() {
            "(no new output)".to_string()
        } else {
            fresh
        };
        Ok(format!("{body}\n{status_line}"))
    }

    /// Block until the shell exits (returns immediately if it already has),
    /// then return its remaining output and exit line. The bash analog of
    /// subagent join.
    pub async fn bash_wait(&self, id: &str, session_id: &str) -> Result<String, BoughError> {
        let shell = self.require(id, session_id)?;
        shell.state.lock().unwrap().claimed = true; // result taken in band
        if shell.status().is_none() {
            shell.wait_exit().await;
        }
        shell.wait_pumps().await;
        self.bash_output(id, session_id)
    }

    /// SIGTERM the shell (graceful for servers that forward it) with a
    /// SIGKILL backstop. Waits for the process to actually die, so the result
    /// reports the real outcome rather than the intent.
    pub async fn bash_kill(&self, id: &str, session_id: &str) -> Result<String, BoughError> {
        let shell = self.require(id, session_id)?;
        if let Some(status) = shell.status() {
            return Ok(format!("{id} already exited with code {}", status.code));
        }
        {
            let mut st = shell.state.lock().unwrap();
            st.claimed = true; // a deliberate kill — don't also post a note
            st.killed = true;
        }
        signal_tree(&shell, Signal::SIGTERM);
        // Backstop for processes that ignore SIGTERM.
        let backstop = {
            let sh = shell.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(KILL_GRACE_MS)).await;
                if sh.status().is_none() {
                    signal_tree(&sh, Signal::SIGKILL);
                }
            })
        };
        let status = shell.wait_exit().await; // bounded: SIGKILL lands after grace
        backstop.abort();
        shell.wait_pumps().await;
        Ok(format!(
            "killed {id} ({})",
            status
                .signal
                .unwrap_or_else(|| format!("exit {}", status.code))
        ))
    }

    // -- the jobs API surface -------------------------------------------------

    /// The session's jobs: everything running, plus shells that ended within
    /// `RECENT_MS`. Running first, then newest.
    pub fn list_jobs(&self, session_id: &str) -> Vec<BackgroundJob> {
        let inner = self.inner.lock().unwrap();
        let Some(shells) = inner.sessions.get(session_id) else {
            return Vec::new();
        };
        let now = (self.now)();
        let mut rows: Vec<(bool, i64, BackgroundJob)> = shells
            .iter()
            .filter_map(|s| {
                let st = s.state.lock().unwrap();
                let keep = st.status.is_none()
                    || st.ended_at.map(|e| now - e < RECENT_MS).unwrap_or(false);
                if keep {
                    Some((st.status.is_none(), st.started_at, job_info_locked(s, &st)))
                } else {
                    None
                }
            })
            .collect();
        rows.sort_by(|a, b| match (a.0, b.0) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b.1.cmp(&a.1),
        });
        rows.into_iter().map(|(_, _, j)| j).collect()
    }

    /// The last `lines` lines of a shell's buffer. Non-destructive: a human
    /// glancing at a log must not make that output vanish from the agent's
    /// next `bashOutput`.
    pub fn job_tail(&self, id: &str, lines: usize) -> Option<JobTail> {
        let shell = self.find(id)?;
        let body = shell_text(&shell).trim_end().to_string();
        if body.is_empty() {
            return Some(JobTail {
                tail: Vec::new(),
                output_lines: 0,
            });
        }
        let all: Vec<&str> = body.split('\n').collect();
        let start = all.len().saturating_sub(lines);
        Some(JobTail {
            tail: all[start..].iter().map(|s| s.to_string()).collect(),
            output_lines: all.len(),
        })
    }

    /// The shell's whole retained buffer, for the jobs tab's output view.
    /// Deliberately does NOT advance `read_to`.
    pub fn job_output(&self, id: &str) -> Option<(String, BackgroundJob)> {
        let shell = self.find(id)?;
        let output = shell_text(&shell).trim_end().to_string();
        let job = job_info(&shell);
        Some((output, job))
    }

    /// Kill by id alone — the UI's kill path. Anything the UI can *list* it
    /// must also be able to read and kill.
    pub async fn kill_job(&self, id: &str) -> Result<String, BoughError> {
        let Some(shell) = self.find(id) else {
            return Err(BoughError::not_found(format!(
                "no background shell {id} — it may have aged out of the job list, or belong \
                 to no session this server knows about.",
            )));
        };
        let session_id = shell.state.lock().unwrap().session_id.clone();
        self.bash_kill(id, &session_id).await
    }

    /// Wait for every tracked shell's pipes to finish draining.
    pub async fn drain(&self) {
        let shells: Vec<Arc<Shell>> = {
            let inner = self.inner.lock().unwrap();
            inner.sessions.values().flatten().cloned().collect()
        };
        for shell in shells {
            shell.wait_exit().await;
            shell.wait_pumps().await;
        }
    }

    /// Ids of the session's still-running shells.
    pub fn running_ids(&self, session_id: &str) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner
            .sessions
            .get(session_id)
            .map(|shells| {
                shells
                    .iter()
                    .filter(|s| s.status().is_none())
                    .map(|s| s.id())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// SIGTERM the session's running shells — "stop everything in this
    /// conversation" reaching background work. Best-effort and synchronous.
    pub fn kill_jobs_of(&self, session_id: &str) -> usize {
        let shells: Vec<Arc<Shell>> = {
            let inner = self.inner.lock().unwrap();
            inner.sessions.get(session_id).cloned().unwrap_or_default()
        };
        let mut n = 0;
        for shell in shells {
            {
                let mut st = shell.state.lock().unwrap();
                if st.status.is_some() {
                    continue;
                }
                st.killed = true;
                st.claimed = true; // a deliberate stop — no wake note
            }
            signal_tree(&shell, Signal::SIGTERM);
            n += 1;
        }
        n
    }

    /// SIGTERM every running shell, for server shutdown.
    ///
    /// Background shells are in-memory by design, so killing the server must
    /// take their processes with it. A SILENT shell (a bare sleep, an idle
    /// dev server) never touches its broken pipe, survives SIGPIPE, and would
    /// be reparented invisibly.
    pub fn kill_all(&self) -> usize {
        let session_ids: Vec<String> = {
            self.inner
                .lock()
                .unwrap()
                .sessions
                .keys()
                .cloned()
                .collect()
        };
        session_ids.iter().map(|sid| self.kill_jobs_of(sid)).sum()
    }

    // -- foreground tracking (interrupt-time partial output) -------------------

    /// Track a foreground shell for the duration of a `bash` call. Returns
    /// the untrack thunk.
    ///
    /// An interrupt terminates the program's worker before the host call can
    /// return, so output the command already produced would vanish with it.
    /// The turn runner reads these buffers at interrupt time instead.
    pub fn track_foreground(
        self: &Arc<Self>,
        shell: &Arc<Shell>,
        session_id: &str,
    ) -> impl FnOnce() + Send {
        {
            let mut inner = self.inner.lock().unwrap();
            inner
                .foreground
                .entry(session_id.to_string())
                .or_default()
                .push(shell.clone());
        }
        let registry = self.clone();
        let shell = shell.clone();
        let session_id = session_id.to_string();
        move || {
            let mut inner = registry.inner.lock().unwrap();
            if let Some(set) = inner.foreground.get_mut(&session_id) {
                set.retain(|s| !Arc::ptr_eq(s, &shell));
                if set.is_empty() {
                    inner.foreground.remove(&session_id);
                }
            }
        }
    }

    /// Partial output of this session's in-flight foreground `bash` calls,
    /// one block per command, or `None` when there is none. Read-only.
    pub fn inflight_foreground_output(&self, session_id: &str) -> Option<String> {
        let shells: Vec<Arc<Shell>> = {
            let inner = self.inner.lock().unwrap();
            inner.foreground.get(session_id)?.clone()
        };
        let blocks: Vec<String> = shells
            .iter()
            .filter_map(|shell| {
                let body = shell_text(shell).trim_end().to_string();
                if body.is_empty() {
                    return None;
                }
                Some(format!(
                    "[interrupted] bash \"{}\" — output before the interrupt:\n{body}",
                    take_chars(&shell.command, 60),
                ))
            })
            .collect();
        if blocks.is_empty() {
            None
        } else {
            Some(blocks.join("\n"))
        }
    }

    // -- internals ------------------------------------------------------------

    /// Lookup across every session — the jobs API aggregates subagent rows.
    fn find(&self, id: &str) -> Option<Arc<Shell>> {
        let inner = self.inner.lock().unwrap();
        inner
            .sessions
            .values()
            .flatten()
            .find(|s| s.state.lock().unwrap().id == id)
            .cloned()
    }

    /// The session-scoped lookup the model's verbs use. Names the ids that DO
    /// exist, because the usual cause is a copied id from another session's
    /// transcript and "not found" alone gives the next round nothing to act
    /// on.
    fn require(&self, id: &str, session_id: &str) -> Result<Arc<Shell>, BoughError> {
        let inner = self.inner.lock().unwrap();
        let shells = inner.sessions.get(session_id);
        if let Some(shell) =
            shells.and_then(|v| v.iter().find(|s| s.state.lock().unwrap().id == id))
        {
            return Ok(shell.clone());
        }
        let known: Vec<String> = shells
            .map(|v| {
                v.iter()
                    .map(|s| {
                        let st = s.state.lock().unwrap();
                        if st.name.is_empty() {
                            st.id.clone()
                        } else {
                            format!("{} \"{}\"", st.id, st.name)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Err(BoughError::not_found(format!(
            "no background shell {id} in this session{}",
            if known.is_empty() {
                " — this session has started none; bashBg(name, cmd) starts one, and a bash() \
                 command that runs past the background threshold reports the id it was \
                 moved to."
                    .to_string()
            } else {
                format!(" — this session has {}.", known.join(", "))
            }
        )))
    }

    fn emit(&self, event_type: EventType, shell: &Arc<Shell>) {
        let bus = self.bus.lock().unwrap().clone();
        let Some(bus) = bus else { return };
        let job = job_info(shell);
        bus.publish(EventInput {
            r#type: event_type,
            session_id: Some(job.session_id.clone()),
            data: serde_json::to_value(&job).unwrap_or(serde_json::Value::Null),
        });
    }

    /// The exit announcement: `job.exited`, then — unless claimed or already
    /// noted — the `[background]` system note. A clean, silent exit (code 0,
    /// no signal, zero output lines) posts NO note: it would wake an idle
    /// session into a paid turn just to say "bg_N finished".
    fn handle_on_exit(&self, shell: &Arc<Shell>) {
        if shell.exit_handled.swap(true, Ordering::SeqCst) {
            return;
        }
        {
            let _g = self.events_lock.lock().unwrap();
            self.emit(EventType::JobExited, shell);
        }
        let notify = self.notify.lock().unwrap().clone();
        let Some(notify) = notify else { return };
        let note = {
            let mut st = shell.state.lock().unwrap();
            if st.notified || st.claimed {
                return;
            }
            st.notified = true;
            let (body, _) = retained_from(&st, 0);
            let body = body.trim_end();
            let lines = if body.is_empty() {
                0
            } else {
                body.split('\n').filter(|l| !l.is_empty()).count()
            };
            let code = st.status.as_ref().map(|s| s.code);
            let signal = st.status.as_ref().and_then(|s| s.signal.clone());
            if code.unwrap_or(0) == 0 && signal.is_none() && lines == 0 {
                return;
            }
            let code_text = code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string());
            (
                st.session_id.clone(),
                format!(
                    "[background] {} \"{}\" finished (exit {code_text}{}) — command \"{}\", \
                     {lines} line{} of output. Read it with bashOutput(\"{}\").",
                    st.id,
                    st.name,
                    signal.map(|s| format!(" on {s}")).unwrap_or_default(),
                    take_chars(&shell.command, 60),
                    if lines == 1 { "" } else { "s" },
                    st.id,
                ),
            )
        };
        notify(&note.0, &note.1);
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn running_count(inner: &Inner, session_id: &str) -> usize {
    inner
        .sessions
        .get(session_id)
        .map(|v| v.iter().filter(|s| s.status().is_none()).count())
        .unwrap_or(0)
}

/// The wire shape of a shell. `BackgroundJob` has only `running` and
/// `exited`, so a killed shell reports as `exited` — its exit code and the
/// `killed` flag carry the distinction internally, and `bash_kill`'s return
/// string carries it to the model.
fn job_info(shell: &Arc<Shell>) -> BackgroundJob {
    let st = shell.state.lock().unwrap();
    job_info_locked(shell, &st)
}

fn job_info_locked(shell: &Arc<Shell>, st: &ShellState) -> BackgroundJob {
    BackgroundJob {
        id: st.id.clone(),
        name: st.name.clone(),
        session_id: st.session_id.clone(),
        pid: shell.pid as i64,
        command: shell.command.clone(),
        status: if st.status.is_none() {
            JobStatus::Running
        } else {
            JobStatus::Exited
        },
        exit_code: st.status.as_ref().map(|s| s.code),
        signal: st.status.as_ref().and_then(|s| s.signal.clone()),
        started_at: st.started_at,
        exited_at: st.ended_at,
    }
}

// ---------------------------------------------------------------------------
// Pumps
// ---------------------------------------------------------------------------

async fn pump<R: tokio::io::AsyncRead + Unpin>(mut stream: R, shell: Arc<Shell>) {
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = [0u8; 16384];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break, // the stream broke with the process
            Ok(n) => {
                carry.extend_from_slice(&buf[..n]);
                let text = drain_decodable(&mut carry);
                if !text.is_empty() {
                    append(&shell, &text);
                }
            }
        }
    }
    if !carry.is_empty() {
        let text = String::from_utf8_lossy(&carry).into_owned();
        append(&shell, &text);
    }
}

/// Incremental UTF-8 decode: everything decodable now, keeping a truncated
/// trailing sequence in `carry` for the next chunk (a chunk boundary must not
/// shred a multi-byte character into replacement marks).
fn drain_decodable(carry: &mut Vec<u8>) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(carry) {
            Ok(s) => {
                out.push_str(s);
                carry.clear();
                return out;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&carry[..valid]).unwrap());
                match e.error_len() {
                    Some(bad) => {
                        out.push('\u{FFFD}');
                        carry.drain(..valid + bad);
                    }
                    None => {
                        // A truncated tail — wait for the next chunk.
                        carry.drain(..valid);
                        return out;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Process trees
// ---------------------------------------------------------------------------

/// Every descendant pid of `root`, deepest first.
///
/// Signalling the shell is not enough: `sh -c 'sleep 900'` does not forward
/// SIGTERM to its foreground child, so killing the shell orphaned the
/// grandchild. macOS has no reliable group-wide answer here; the portable
/// path is to read the tree out of `ps`. Synchronous because a shutdown
/// handler has no chance to await.
pub fn descendant_pids(root: i32) -> Vec<i32> {
    let output = match std::process::Command::new("ps")
        .args(["-Ao", "pid=,ppid="])
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(), // no ps: signal the shell alone
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut kids: HashMap<i32, Vec<i32>> = HashMap::new();
    for line in text.split('\n') {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (it.next(), it.next()) else {
            continue;
        };
        if it.next().is_some() {
            continue;
        }
        let (Ok(pid), Ok(ppid)) = (pid.parse::<i32>(), ppid.parse::<i32>()) else {
            continue;
        };
        kids.entry(ppid).or_default().push(pid);
    }
    let mut out: Vec<i32> = Vec::new();
    let mut seen: std::collections::HashSet<i32> = std::collections::HashSet::from([root]);
    fn walk(
        p: i32,
        kids: &HashMap<i32, Vec<i32>>,
        seen: &mut std::collections::HashSet<i32>,
        out: &mut Vec<i32>,
    ) {
        for &c in kids.get(&p).map(|v| v.as_slice()).unwrap_or(&[]) {
            if seen.contains(&c) {
                continue; // ps raced a reparent; don't loop
            }
            seen.insert(c);
            walk(c, kids, seen, out);
            out.push(c);
        }
    }
    walk(root, &kids, &mut seen, &mut out);
    out
}

/// Signal the shell AND everything it spawned, descendants first so a parent
/// cannot restart one after we have passed it. Every kill error swallowed.
pub fn signal_tree(shell: &Shell, sig: Signal) {
    let self_pid = std::process::id() as i32;
    for pid in descendant_pids(shell.pid) {
        if pid <= 1 || pid == self_pid {
            continue; // never signal init or ourselves
        }
        let _ = nix::sys::signal::kill(Pid::from_raw(pid), sig);
    }
    let _ = nix::sys::signal::kill(Pid::from_raw(shell.pid), sig); // raced a natural exit
}

fn exit_status_of(st: std::process::ExitStatus) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus {
        code: st.code().unwrap_or(0) as i64,
        signal: st.signal().map(signal_name),
    }
}

fn signal_name(n: i32) -> String {
    match Signal::try_from(n) {
        Ok(sig) => sig.as_str().to_string(),
        Err(_) => format!("SIG{n}"),
    }
}

// ---------------------------------------------------------------------------
// char-offset helpers ("chars" are Unicode scalar values; TS counted UTF-16
// units — the two agree on ASCII, which shell output overwhelmingly is)
// ---------------------------------------------------------------------------

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn skip_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

// ---------------------------------------------------------------------------
// Tests — the pure pieces. Subprocess-driving tests (spawn, kill-tree,
// buffers over a live pipe) live in shell.rs's test module beside the verbs
// that exercise them.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_name_strips_preludes_and_assignments() {
        assert_eq!(
            derive_name("cd /src/bough && npm run build -- --watch"),
            "npm run build -- --watch"
        );
        assert_eq!(derive_name("NODE_ENV=test sleep 60"), "sleep 60");
        assert_eq!(derive_name("   "), "shell");
        assert_eq!(derive_name("printf a | wc -l"), "printf a");
    }

    #[test]
    fn normalize_job_name_collapses_and_caps() {
        assert_eq!(
            normalize_job_name("  dev   server \n").as_deref(),
            Some("dev server")
        );
        assert_eq!(normalize_job_name(""), None);
        assert_eq!(normalize_job_name("   "), None);
        assert_eq!(normalize_job_name("\n\t"), None);
        let long = "x".repeat(80);
        let capped = normalize_job_name(&long).unwrap();
        assert_eq!(capped.chars().count(), 60);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn retained_buffer_keeps_head_then_rolls_tail() {
        let mut st = ShellState {
            id: String::new(),
            name: String::new(),
            session_id: String::new(),
            started_at: 0,
            ended_at: None,
            killed: false,
            head: String::new(),
            tail: String::new(),
            written: 0,
            sink: None,
            read_to: 0,
            status: None,
            claimed: false,
            notified: false,
        };
        let limits = ResolvedLimits { head: 4, tail: 4 };
        for chunk in ["abc", "def", "ghi", "jkl"] {
            st.written += char_len(chunk);
            retain(&mut st, limits, chunk);
        }
        assert_eq!(st.head, "abcd"); // fills once, immutable when full
        assert_eq!(st.tail, "ijkl"); // rolls
        assert_eq!(st.written, 12);
        let (text, omitted) = retained_from(&st, 0);
        assert!(text.starts_with("abcd"));
        assert!(text.ends_with("ijkl"));
        assert!(text.contains("4 chars omitted from the middle of 12"));
        assert_eq!(omitted, 4);
    }

    #[test]
    fn retained_from_reads_deltas_by_absolute_offset() {
        let mut st = ShellState {
            id: String::new(),
            name: String::new(),
            session_id: String::new(),
            started_at: 0,
            ended_at: None,
            killed: false,
            head: "abcdef".to_string(),
            tail: String::new(),
            written: 6,
            sink: None,
            read_to: 0,
            status: None,
            claimed: false,
            notified: false,
        };
        let (first, _) = retained_from(&st, 0);
        assert_eq!(first, "abcdef");
        let (delta, _) = retained_from(&st, 4);
        assert_eq!(delta, "ef");
        st.head.push_str("gh");
        st.written = 8;
        let (next, _) = retained_from(&st, 6);
        assert_eq!(next, "gh");
    }

    #[test]
    fn descendant_pids_returns_nothing_for_a_leaf() {
        // A pid with no children (ours may have some under a test harness,
        // so probe an unlikely-to-exist one).
        assert_eq!(descendant_pids(-31337), Vec::<i32>::new());
    }
}
