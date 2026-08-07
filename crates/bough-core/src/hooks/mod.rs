//! Hooks: Lua that runs inside bough's own lifecycle and is allowed to change
//! what happens next.
//!
//! THE SHAPE, AND WHY IT IS THIS ONE. Two designs exist in the wild. Claude
//! Code shells out and reads a JSON patch off stdout: process-isolated and
//! auditable, but every new power needs a new field in a schema somebody owns.
//! maki embeds Lua and hands plugins real host APIs (`maki.session.prompt`,
//! prompt slots, `create_autocmd`): open-ended and composable, at the cost of
//! running foreign code in-process. This is the second one, because a hook
//! that can only return a patch is a filter, and the ask was for hooks that
//! are first-class — able to start work, not just veto it.
//!
//! THE EVENT SURFACE IS SMALL ON PURPOSE. maki fires four events; Claude Code
//! fires thirty. Four is the better number: every event is a compatibility
//! promise, and the ones that earn their keep are the turn boundaries and the
//! tool boundary. Plugins can define their own with `exec_autocmds`.
//!
//! ## Two kinds of change, and the line between them
//!
//! **Returned** — a callback's return value decides the thing that is
//! happening right now: deny a command, rewrite its input, replace its
//! output, stop the turn. Synchronous by nature; the caller is blocked on the
//! answer.
//!
//! **Effected** — `bough.session.prompt(...)` and `bough.session.set_title(...)`
//! change the session itself. These cannot be applied from inside the Lua
//! call: the database and the turn starter live on the async side, and a hook
//! thread reaching into them would deadlock against the turn it is running
//! inside. So they are RECORDED as [`Effect`]s during dispatch and applied by
//! the caller afterwards, which also makes them testable without a database.
//!
//! ## The isolation rules
//!
//! - **One thread owns the interpreter.** `mlua::Lua` is not `Sync` and hook
//!   callbacks are arbitrary user code; the state lives on a dedicated thread
//!   and every dispatch is a channel round trip. Nothing else can touch it.
//! - **A hook cannot wedge a turn.** Every dispatch carries a deadline
//!   enforced by a Luau interrupt, so an infinite loop in a hook costs one
//!   event and a log line.
//! - **A broken hook is a log line, never a failed turn.** Load errors,
//!   runtime errors and timeouts all degrade to "this hook contributed
//!   nothing". The one exception is deliberate: a hook that returns
//!   `{ stop = "reason" }` meant to stop the turn.
//! - **Absent by default.** No `~/.bough/hooks` directory means no thread, no
//!   interpreter, and no cost.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::paths::bough_path;

mod runtime;

#[cfg(test)]
mod tests;

/// How long one dispatch may run before the interpreter is interrupted.
/// Generous for a hook that shells out, short enough that a runaway one is a
/// hiccup rather than a hang.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Where hooks are loaded from: every `*.lua` file directly inside, in name
/// order so two hooks that both wrap something have a defined order.
pub fn hooks_dir() -> PathBuf {
    bough_path(&["hooks"])
}

// ---------------------------------------------------------------------------
// The wire between bough and a hook
// ---------------------------------------------------------------------------

/// The events the host fires. Plugin-defined events travel as
/// [`HookEvent::Custom`] and never reach the host's own dispatch sites.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    /// A turn is about to run. `data.prompt` is the user's message.
    TurnStart,
    /// A turn finished cleanly.
    TurnEnd,
    /// A turn ended in an error. `data.error` is the message.
    TurnError,
    /// A shell command is about to run. Returnable: `decision`, `input`.
    PreTool,
    /// A shell command finished. Returnable: `output`.
    PostTool,
    /// Fired by a plugin through `exec_autocmds`.
    Custom(String),
}

impl HookEvent {
    pub fn name(&self) -> &str {
        match self {
            HookEvent::TurnStart => "TurnStart",
            HookEvent::TurnEnd => "TurnEnd",
            HookEvent::TurnError => "TurnError",
            HookEvent::PreTool => "PreTool",
            HookEvent::PostTool => "PostTool",
            HookEvent::Custom(name) => name,
        }
    }

    pub fn parse(name: &str) -> HookEvent {
        match name {
            "TurnStart" => HookEvent::TurnStart,
            "TurnEnd" => HookEvent::TurnEnd,
            "TurnError" => HookEvent::TurnError,
            "PreTool" => HookEvent::PreTool,
            "PostTool" => HookEvent::PostTool,
            other => HookEvent::Custom(other.to_string()),
        }
    }
}

/// What a hook wants done to the tool call it is inspecting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolDecision {
    /// Run it, skipping any permission prompt this command would have raised.
    Allow,
    /// Refuse it. The reason goes to the model as the tool's result, so the
    /// next round can act on it — a denial the model cannot read is a mystery
    /// it will retry.
    Deny,
}

/// A change to the session itself, recorded during dispatch and applied by
/// the caller. See the module header for why these are not applied inline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    /// `bough.session.prompt(text)` — inject a user turn. Starts a turn on an
    /// idle session and queues behind a running one; the wake rule belongs to
    /// `agents/notes.rs` and is not re-decided here.
    Prompt { text: String },
    /// `bough.session.set_title(title)`.
    SetTitle { title: String },
}

/// Everything one dispatch produced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookOutcome {
    /// `bough.context(text)` — text for the model to read. The caller decides
    /// where it lands, because that differs by event: turn-start context can
    /// ride the prompt, mid-turn context must ride the round's result or it
    /// busts the volatile tier's cache (`prompt/assemble.rs`).
    pub context: Vec<String>,
    /// `{ decision = "deny", reason = "..." }` from a PreTool hook.
    pub decision: Option<ToolDecision>,
    pub reason: Option<String>,
    /// `{ input = {...} }` — the tool call's arguments, rewritten.
    pub input: Option<serde_json::Value>,
    /// `{ output = "..." }` — the tool's result, rewritten.
    pub output: Option<String>,
    /// `bough.stop(reason)` or `{ stop = "..." }` — end the turn.
    pub stop: Option<String>,
    /// Session changes for the caller to apply, in the order they were made.
    pub effects: Vec<Effect>,
    /// What went wrong, for the caller to log. Never empty on a failed hook,
    /// and never a reason to fail the turn.
    pub errors: Vec<String>,
}

impl HookOutcome {
    /// What this outcome DID, as short verbs — the announcement's payload and
    /// the panel's "last" column.
    pub fn verbs(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if !self.context.is_empty() {
            out.push("added context".into());
        }
        match self.decision {
            Some(ToolDecision::Deny) => out.push("denied a command".into()),
            Some(ToolDecision::Allow) => out.push("allowed a command".into()),
            None => {}
        }
        if self.input.is_some() {
            out.push("rewrote a command".into());
        }
        if self.output.is_some() {
            out.push("rewrote output".into());
        }
        for effect in &self.effects {
            out.push(match effect {
                Effect::Prompt { .. } => "sent a prompt".into(),
                Effect::SetTitle { .. } => "renamed the session".into(),
            });
        }
        if self.stop.is_some() {
            out.push("stopped the turn".into());
        }
        out
    }

    /// Did anything at all come back? Callers skip their apply path when not.
    pub fn is_empty(&self) -> bool {
        self.context.is_empty()
            && self.decision.is_none()
            && self.input.is_none()
            && self.output.is_none()
            && self.stop.is_none()
            && self.effects.is_empty()
    }
}

/// One event, as the host hands it over.
#[derive(Clone, Debug, Default)]
pub struct HookDispatch {
    pub session_id: String,
    /// Matched against each autocmd's `pattern`. The tool name for the tool
    /// events, the session id otherwise.
    pub pattern: String,
    /// `ev.data` inside the callback.
    pub data: serde_json::Value,
}

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

pub(crate) enum Request {
    Dispatch {
        event: HookEvent,
        dispatch: HookDispatch,
        reply: SyncSender<HookOutcome>,
    },
    /// Test seam: how many autocmds are registered, so a load can be asserted
    /// without firing anything.
    Count(SyncSender<usize>),
    /// Per-file `(fired, last)`.
    Activity(SyncSender<std::collections::HashMap<String, (u64, Option<String>)>>),
    /// How many listeners one file registered.
    ListenersFor {
        file: String,
        reply: SyncSender<usize>,
    },
    Shutdown,
}

/// A loaded set of hooks and the thread that runs them.
pub struct HookHost {
    tx: Mutex<SyncSender<Request>>,
    /// Files that were loaded, in load order — the panel and the logs name
    /// them.
    pub loaded: Vec<PathBuf>,
    /// Files that failed to load, with why. Reported, never fatal.
    pub failed: Vec<(PathBuf, String)>,
}

impl HookHost {
    /// Load every `*.lua` in `dir`, or `None` when there is nothing to load —
    /// no directory, or no Lua in it. `None` is the everyday answer and costs
    /// nothing: no thread and no interpreter.
    pub fn load(dir: &Path) -> Option<HookHost> {
        HookHost::load_enabled(dir, Path::new("/nonexistent-disabled-list"))
    }

    /// [`HookHost::load`], skipping the files named in the disabled list.
    pub fn load_enabled(dir: &Path, disabled_at: &Path) -> Option<HookHost> {
        let disabled = read_disabled(disabled_at);
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "lua") && p.is_file())
            .filter(|p| {
                !p.file_name()
                    .map(|n| disabled.contains(&n.to_string_lossy().into_owned()))
                    .unwrap_or(false)
            })
            .collect();
        if files.is_empty() {
            return None;
        }
        // Name order, so two hooks that both decide something have an order
        // the user can see in `ls`.
        files.sort();

        let (tx, rx) = sync_channel::<Request>(0);
        let (ready_tx, ready_rx) = sync_channel::<(Vec<PathBuf>, Vec<(PathBuf, String)>)>(0);
        let to_load = files.clone();
        std::thread::Builder::new()
            .name("bough-hooks".into())
            .spawn(move || runtime::serve(to_load, rx, ready_tx))
            .ok()?;
        let (loaded, failed) = ready_rx.recv().ok()?;
        Some(HookHost {
            tx: Mutex::new(tx),
            loaded,
            failed,
        })
    }

    /// Fire an event and collect what the hooks did with it.
    ///
    /// Blocking, bounded by [`DISPATCH_TIMEOUT`] plus a slack: async callers
    /// wrap this in `spawn_blocking`. A dead or wedged hook thread yields an
    /// empty outcome rather than an error, because there is no dispatch site
    /// where failing the work is better than skipping the hook.
    pub fn dispatch(&self, event: HookEvent, dispatch: HookDispatch) -> HookOutcome {
        let (reply, answer) = sync_channel::<HookOutcome>(0);
        let sent = self.tx.lock().ok().and_then(|tx| {
            tx.send(Request::Dispatch {
                event,
                dispatch,
                reply,
            })
            .ok()
        });
        if sent.is_none() {
            return HookOutcome::default();
        }
        answer
            .recv_timeout(DISPATCH_TIMEOUT + Duration::from_secs(1))
            .unwrap_or_default()
    }

    /// How many listeners one loaded file registered — the panel's per-row
    /// count, so a hook that loaded but wired nothing is visibly different
    /// from one that wired three.
    pub fn listeners_for(&self, path: &Path) -> usize {
        let (reply, answer) = sync_channel::<usize>(0);
        let sent = self.tx.lock().ok().and_then(|tx| {
            tx.send(Request::ListenersFor {
                file: path.to_string_lossy().into_owned(),
                reply,
            })
            .ok()
        });
        if sent.is_none() {
            return 0;
        }
        answer.recv_timeout(DISPATCH_TIMEOUT).unwrap_or(0)
    }

    /// What each loaded file has done since it loaded.
    pub fn activity(&self) -> std::collections::HashMap<String, (u64, Option<String>)> {
        let (reply, answer) = sync_channel(0);
        let sent = self
            .tx
            .lock()
            .ok()
            .and_then(|tx| tx.send(Request::Activity(reply)).ok());
        if sent.is_none() {
            return Default::default();
        }
        answer.recv_timeout(DISPATCH_TIMEOUT).unwrap_or_default()
    }

    /// How many autocmds the loaded hooks registered.
    pub fn autocmd_count(&self) -> usize {
        let (reply, answer) = sync_channel::<usize>(0);
        let sent = self
            .tx
            .lock()
            .ok()
            .and_then(|tx| tx.send(Request::Count(reply)).ok());
        if sent.is_none() {
            return 0;
        }
        answer.recv_timeout(DISPATCH_TIMEOUT).unwrap_or(0)
    }
}

impl Drop for HookHost {
    fn drop(&mut self) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(Request::Shutdown);
        }
    }
}

// ---------------------------------------------------------------------------
// Which hooks are on
// ---------------------------------------------------------------------------

/// Where the off switches live: `~/.bough/hooks-disabled.json`, a plain list
/// of file names.
///
/// A LIST OF WHAT IS OFF, not of what is on. Dropping a new `.lua` into the
/// directory has to work — that is the whole installation story — so the
/// default for an unlisted file is ENABLED, and a file the user has never
/// heard of cannot be silently inert.
pub fn disabled_path() -> PathBuf {
    bough_path(&["hooks-disabled.json"])
}

fn read_disabled(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
}

fn write_disabled(path: &Path, names: &[String]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(names).unwrap_or_else(|_| "[]".into());
    std::fs::write(path, json)
}

/// One hook file, as the panel lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookFile {
    /// The file name — the id the toggle route takes.
    pub name: String,
    pub path: String,
    pub enabled: bool,
    /// Listeners this file registered. 0 on a disabled one (it never ran) and
    /// on one that registered none, which the panel distinguishes by
    /// `enabled`.
    pub autocmds: usize,
    /// How many times this file has acted since it loaded, and what it did
    /// most recently. The panel's activity column: a hook that fires every
    /// turn and one that has never done anything look identical without it.
    #[serde(default)]
    pub fired: u64,
    #[serde(default)]
    pub last: Option<String>,
    /// Why it did not load. A file that fails to parse is LISTED with its
    /// error rather than omitted — the same rule the skills tab holds, and for
    /// the same reason: a hook that silently vanished is discovered as a hook
    /// that quietly did nothing.
    pub error: Option<String>,
}

/// Every `.lua` in the hooks directory, enabled or not, with what the live
/// host knows about the ones that loaded.
pub fn list_hooks() -> Vec<HookFile> {
    list_hooks_in(&hooks_dir(), &disabled_path())
}

pub fn list_hooks_in(dir: &Path, disabled_at: &Path) -> Vec<HookFile> {
    let disabled = read_disabled(disabled_at);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "lua") && p.is_file())
        .collect();
    files.sort();
    let live = host();
    let activity = live.map(|h| h.activity()).unwrap_or_default();
    files
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let enabled = !disabled.contains(&name);
            let error = live.and_then(|h| {
                h.failed
                    .iter()
                    .find(|(p, _)| p == &path)
                    .map(|(_, e)| e.clone())
            });
            let acted = activity.get(&path.to_string_lossy().into_owned()).cloned();
            HookFile {
                name,
                fired: acted.as_ref().map(|(n, _)| *n).unwrap_or(0),
                last: acted.and_then(|(_, l)| l),
                path: path.to_string_lossy().into_owned(),
                enabled,
                autocmds: live
                    .filter(|_| enabled)
                    .map(|h| h.listeners_for(&path))
                    .unwrap_or(0),
                error,
            }
        })
        .collect()
}

/// Turn one hook on or off and rebuild the interpreter.
///
/// A RELOAD, not a flag consulted at dispatch time: a disabled hook must stop
/// existing, not stop being called. Its `create_autocmd` calls are gone, so a
/// listener it registered on somebody else's event is gone too — which a
/// dispatch-time filter could not achieve, because the registration happened
/// at load.
pub fn set_enabled(name: &str, enabled: bool) -> Result<(), std::io::Error> {
    let path = disabled_path();
    let mut disabled = read_disabled(&path);
    let was = !disabled.contains(&name.to_string());
    if was == enabled {
        return Ok(()); // already there; no rebuild for a no-op
    }
    if enabled {
        disabled.retain(|n| n != name);
    } else {
        disabled.push(name.to_string());
        disabled.sort();
    }
    write_disabled(&path, &disabled)?;
    reload();
    Ok(())
}

/// Rebuild the process-wide host from what is on disk right now.
pub fn reload() {
    let dir = hooks_dir();
    let rebuilt = HookHost::load_enabled(&dir, &disabled_path());
    if let Ok(mut slot) = host_slot().write() {
        // The old host drops here, which shuts its thread down. A turn holding
        // a dispatch mid-flight keeps the old one alive until it answers,
        // because `fire` cloned nothing — it borrows under the read lock.
        *slot = Some((dir, rebuilt));
    }
}

/// The built host, and the directory it was built FROM.
///
/// The directory is stored because it can change under the process: tests
/// redirect `BOUGH_HOME`, and a user can too. A host built from one directory
/// answering questions about another is the kind of wrong that reads as a
/// bug in the hook rather than in the cache.
type HostSlot = Option<(PathBuf, Option<HookHost>)>;

fn host_slot() -> &'static std::sync::RwLock<HostSlot> {
    static HOST: OnceLock<std::sync::RwLock<HostSlot>> = OnceLock::new();
    HOST.get_or_init(|| std::sync::RwLock::new(None))
}

/// The process-wide host over `~/.bough/hooks`.
///
/// Loaded on first use and rebuilt only by [`reload`] — never per turn, so a
/// half-written file cannot change behaviour underneath a running turn, and a
/// machine with no hooks pays one directory read for the life of the process.
///
/// The `&'static` is real: the host lives in a leaked box once built, because
/// callers hold it across a blocking dispatch and a lock guard cannot cross
/// that boundary. A reload leaks the previous one — bounded by how many times
/// a human toggles a hook, which is not a leak that matters.
pub fn host() -> Option<&'static HookHost> {
    let dir = hooks_dir();
    {
        let slot = host_slot().read().ok()?;
        if let Some((built_from, built)) = slot.as_ref() {
            if built_from == &dir {
                return built.as_ref().map(|h| unsafe { extend(h) });
            }
        }
    }
    let mut slot = host_slot().write().ok()?;
    let stale = slot.as_ref().map(|(d, _)| d != &dir).unwrap_or(true);
    if stale {
        *slot = Some((dir.clone(), HookHost::load_enabled(&dir, &disabled_path())));
    }
    slot.as_ref()
        .and_then(|(_, b)| b.as_ref())
        .map(|h| unsafe { extend(h) })
}

/// SAFETY: the host is only ever dropped by [`reload`], which replaces the
/// slot; the box it lives in is leaked there, so a reference handed out here
/// stays valid for the life of the process.
unsafe fn extend(h: &HookHost) -> &'static HookHost {
    std::mem::transmute::<&HookHost, &'static HookHost>(h)
}

/// Fire an event at the process-wide host. `None` when no hooks are loaded,
/// which every caller treats as "nothing to apply".
pub fn fire(event: HookEvent, dispatch: HookDispatch) -> Option<HookOutcome> {
    fire_on(None, event, dispatch)
}

/// [`fire`], announcing what the hooks did on `bus`.
///
/// ANNOUNCED, ALWAYS. A hook that rewrites a command or injects context is
/// changing what the user sees the agent do, and an unexplained change reads
/// as the harness misbehaving. The event carries the VERBS, not the content —
/// the content already lands where it belongs (a note in the transcript, the
/// command's own result), and repeating it in a toast would say everything
/// twice.
pub fn fire_on(
    bus: Option<&Arc<crate::bus::Bus>>,
    event: HookEvent,
    dispatch: HookDispatch,
) -> Option<HookOutcome> {
    let host = host()?;
    let session_id = dispatch.session_id.clone();
    let name = event.name().to_string();
    let outcome = host.dispatch(event, dispatch);
    for err in &outcome.errors {
        tracing::warn!("hook: {err}");
    }
    if outcome.is_empty() && outcome.errors.is_empty() {
        return None;
    }
    if let Some(bus) = bus {
        let actions = outcome.verbs();
        // An error with no action is still worth announcing: a hook that
        // throws every turn is invisible otherwise, and "why did nothing
        // happen" is the hardest question this subsystem can pose.
        bus.publish(crate::schema::events::EventInput {
            r#type: crate::schema::events::EventType::HookFired,
            session_id: Some(session_id),
            data: serde_json::json!({
                "event": name,
                "actions": actions,
                "errors": outcome.errors.len(),
            }),
        });
    }
    if outcome.is_empty() {
        return None;
    }
    Some(outcome)
}

/// Shared deadline handed to the interrupt callback.
#[derive(Clone)]
pub(crate) struct Deadline(pub(crate) Arc<Mutex<Instant>>);

impl Deadline {
    pub(crate) fn new() -> Deadline {
        Deadline(Arc::new(Mutex::new(Instant::now() + DISPATCH_TIMEOUT)))
    }

    pub(crate) fn arm(&self) {
        if let Ok(mut at) = self.0.lock() {
            *at = Instant::now() + DISPATCH_TIMEOUT;
        }
    }

    pub(crate) fn expired(&self) -> bool {
        self.0
            .lock()
            .map(|at| Instant::now() > *at)
            .unwrap_or(false)
    }
}

pub(crate) type ReqRx = Receiver<Request>;
pub(crate) use Request as HookRequest;
