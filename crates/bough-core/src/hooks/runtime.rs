//! The interpreter thread: the `bough` global, the autocmd registry, and the
//! dispatch loop.
//!
//! Everything here runs on ONE thread and never touches the database, the
//! bus, or a turn. What a hook wants done to the session leaves as an
//! [`Effect`] on the outcome; what it wants done to the tool call in flight
//! leaves as a returned field. That is the whole boundary, and it is what
//! makes a hook unable to deadlock against the turn it is running inside.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;

use mlua::{Lua, LuaSerdeExt, Table, Value, VmState};

use super::{
    Deadline, Effect, HookDispatch, HookEvent, HookOutcome, HookRequest, ReqRx, ToolDecision,
};

/// One registered listener.
struct Autocmd {
    id: u32,
    /// The file whose top level registered this, so the panel can count per
    /// hook. Empty for one registered from inside a callback at runtime.
    file: String,
    event: String,
    /// `None` or `"*"` matches everything.
    pattern: Option<String>,
    once: bool,
    callback: mlua::RegistryKey,
}

/// What a callback accumulated, shared with the Lua closures through the app
/// data slot. Rust-side state, not Lua state, so a hook cannot forge it.
#[derive(Default)]
struct Collected {
    context: Vec<String>,
    stop: Option<String>,
    effects: Vec<Effect>,
    errors: Vec<String>,
}

/// How much had been collected before one callback ran — lengths, because the
/// vectors only ever grow within a dispatch.
#[derive(Clone, Copy, Default)]
struct Mark {
    context: usize,
    effects: usize,
    stopped: bool,
}

impl Collected {
    fn mark(&self) -> Mark {
        Mark {
            context: self.context.len(),
            effects: self.effects.len(),
            stopped: self.stop.is_some(),
        }
    }

    /// The verbs for what was added since `mark` — one callback's imperative
    /// contribution, separated from every other callback's.
    fn since(&self, mark: &Mark) -> Vec<String> {
        let mut out = Vec::new();
        if self.context.len() > mark.context {
            out.push("added context".to_string());
        }
        for effect in self.effects.iter().skip(mark.effects) {
            out.push(match effect {
                Effect::Prompt { .. } => "sent a prompt".to_string(),
                Effect::SetTitle { .. } => "renamed the session".to_string(),
            });
        }
        if self.stop.is_some() && !mark.stopped {
            out.push("stopped the turn".to_string());
        }
        out
    }
}

/// The thread body: build the interpreter, load the files, then serve.
pub(super) fn serve(
    files: Vec<PathBuf>,
    rx: ReqRx,
    ready: SyncSender<(Vec<PathBuf>, Vec<(PathBuf, String)>)>,
) {
    let lua = Lua::new();
    let deadline = Deadline::new();
    // A hook that loops forever costs one event, not the session. Luau calls
    // the interrupt every few VM instructions.
    let watch = deadline.clone();
    lua.set_interrupt(move |_| {
        if watch.expired() {
            // An ERROR, not `VmState::Yield`. Yield only suspends a coroutine,
            // and a hook callback is called from Rust with nothing to yield
            // to — `while true do end` then runs forever with the interrupt
            // politely asking it to stop. Raising unwinds the callback, which
            // `fire_matching` reports like any other throwing hook.
            return Err(mlua::Error::runtime(
                "hook exceeded its time budget and was interrupted",
            ));
        }
        Ok(VmState::Continue)
    });

    let registry: Registry = Registry::default();
    let state = std::rc::Rc::new(std::cell::RefCell::new(registry));
    if let Err(err) = install_api(&lua, state.clone()) {
        let _ = ready.send((Vec::new(), vec![(PathBuf::from("<api>"), err.to_string())]));
        return;
    }

    let mut loaded = Vec::new();
    let mut failed = Vec::new();
    for file in files {
        deadline.arm();
        state.borrow_mut().loading = file.to_string_lossy().into_owned();
        match std::fs::read_to_string(&file) {
            Ok(src) => match lua.load(&src).set_name(file.to_string_lossy()).exec() {
                Ok(()) => loaded.push(file),
                Err(err) => failed.push((file, err.to_string())),
            },
            Err(err) => failed.push((file, err.to_string())),
        }
    }
    state.borrow_mut().loading = String::new();
    if ready.send((loaded, failed)).is_err() {
        return;
    }

    while let Ok(req) = rx.recv() {
        match req {
            HookRequest::Shutdown => break,
            HookRequest::Count(reply) => {
                let n = state.borrow().autocmds.len();
                let _ = reply.send(n);
            }
            HookRequest::Activity(reply) => {
                let map = state
                    .borrow()
                    .activity
                    .iter()
                    .map(|(k, v)| (k.clone(), (v.fired, v.last.clone())))
                    .collect();
                let _ = reply.send(map);
            }
            HookRequest::ListenersFor { file, reply } => {
                let n = state
                    .borrow()
                    .autocmds
                    .iter()
                    .filter(|a| a.file == file)
                    .count();
                let _ = reply.send(n);
            }
            HookRequest::Dispatch {
                event,
                dispatch,
                reply,
            } => {
                deadline.arm();
                let outcome = run_event(&lua, &state, &event, &dispatch);
                let _ = reply.send(outcome);
            }
        }
    }
}

/// What one file has done since it loaded — the panel's activity column.
#[derive(Default, Clone)]
pub(super) struct Activity {
    pub(super) fired: u64,
    pub(super) last: Option<String>,
}

#[derive(Default)]
struct Registry {
    autocmds: Vec<Autocmd>,
    /// Keyed by file path, so a hook that has never done anything is visibly
    /// different from one that fires every turn.
    activity: HashMap<String, Activity>,
    next_id: u32,
    collected: Collected,
    /// The file currently being loaded — attribution for `create_autocmd`.
    loading: String,
}

type Shared = std::rc::Rc<std::cell::RefCell<Registry>>;

/// Build the `bough` global.
fn install_api(lua: &Lua, state: Shared) -> mlua::Result<()> {
    let bough = lua.create_table()?;

    // ---- bough.api ---------------------------------------------------------
    let api = lua.create_table()?;
    let reg = state.clone();
    api.set(
        "create_autocmd",
        lua.create_function(move |lua, (event, opts): (Value, Table)| {
            let callback: mlua::Function = opts.get("callback")?;
            let once: Option<bool> = opts.get("once")?;
            let pattern: Option<String> = opts.get("pattern").ok().flatten();
            let events = string_list(&event)?;
            let mut r = reg.borrow_mut();
            r.next_id += 1;
            let id = r.next_id;
            // One id for the whole call even when it names several events, so
            // `del_autocmd(id)` removes what `create_autocmd` created.
            for name in events.iter() {
                // One registry value PER listener, even when several events
                // share a callback: removal frees a key, and a shared key
                // would leave the survivors pointing at nothing.
                let key = lua.create_registry_value(callback.clone())?;
                let file = r.loading.clone();
                r.autocmds.push(Autocmd {
                    id,
                    file,
                    event: name.clone(),
                    pattern: pattern.clone(),
                    once: once.unwrap_or(false),
                    callback: key,
                });
            }
            Ok(id)
        })?,
    )?;
    let reg = state.clone();
    api.set(
        "del_autocmd",
        lua.create_function(move |_, id: u32| {
            reg.borrow_mut().autocmds.retain(|a| a.id != id);
            Ok(())
        })?,
    )?;
    let reg = state.clone();
    api.set(
        "exec_autocmds",
        lua.create_function(move |lua, (event, opts): (Value, Option<Table>)| {
            let opts = opts;
            let pattern = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("pattern").ok().flatten())
                .unwrap_or_default();
            let data = opts
                .as_ref()
                .and_then(|o| o.get::<Value>("data").ok())
                .unwrap_or(Value::Nil);
            for name in string_list(&event)? {
                // Plugin-fired events run through the same path as host ones,
                // so a plugin can test its own listeners.
                // A plugin-fired event inherits the workspace of nothing —
                // it is not the host's dispatch — so it carries the empty string.
                let _ = fire_matching(lua, &reg, &name, &pattern, data.clone(), "");
            }
            Ok(())
        })?,
    )?;
    bough.set("api", api)?;

    // ---- bough.log ---------------------------------------------------------
    let log = lua.create_table()?;
    for level in ["debug", "info", "warn", "error"] {
        log.set(
            level,
            lua.create_function(move |_, msg: String| {
                match level {
                    "debug" => tracing::debug!("hook: {msg}"),
                    "warn" => tracing::warn!("hook: {msg}"),
                    "error" => tracing::error!("hook: {msg}"),
                    _ => tracing::info!("hook: {msg}"),
                }
                Ok(())
            })?,
        )?;
    }
    bough.set("log", log)?;

    // ---- bough.context / bough.stop ----------------------------------------
    let reg = state.clone();
    bough.set(
        "context",
        lua.create_function(move |_, text: String| {
            reg.borrow_mut().collected.context.push(text);
            Ok(())
        })?,
    )?;
    let reg = state.clone();
    bough.set(
        "stop",
        lua.create_function(move |_, reason: Option<String>| {
            let mut r = reg.borrow_mut();
            if r.collected.stop.is_none() {
                r.collected.stop = Some(reason.unwrap_or_else(|| "a hook stopped the turn".into()));
            }
            Ok(())
        })?,
    )?;

    // ---- bough.home --------------------------------------------------------
    // The one path a hook cannot derive from the event: adopting another
    // harness means reading ITS user-level config, which lives under $HOME.
    bough.set(
        "home",
        lua.create_function(|_, ()| {
            Ok(dirs::home_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default())
        })?,
    )?;

    // ---- bough.session -----------------------------------------------------
    let session = lua.create_table()?;
    let reg = state.clone();
    session.set(
        "prompt",
        lua.create_function(move |_, text: String| {
            if text.trim().is_empty() {
                // Mirrors maki: a blank prompt is a programmer mistake, and
                // failing loudly here beats a mystery empty turn later.
                return Err(mlua::Error::runtime(
                    "bough.session.prompt: text must not be blank",
                ));
            }
            reg.borrow_mut()
                .collected
                .effects
                .push(Effect::Prompt { text });
            Ok(())
        })?,
    )?;
    let reg = state.clone();
    session.set(
        "set_title",
        lua.create_function(move |_, title: String| {
            reg.borrow_mut()
                .collected
                .effects
                .push(Effect::SetTitle { title });
            Ok(())
        })?,
    )?;
    bough.set("session", session)?;

    // ---- bough.json --------------------------------------------------------
    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|lua, value: Value| {
            let v: serde_json::Value = lua.from_value(value)?;
            serde_json::to_string(&v).map_err(mlua::Error::runtime)
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| mlua::Error::runtime(e.to_string()))?;
            lua.to_value(&v)
        })?,
    )?;
    bough.set("json", json)?;

    // ---- bough.fs ----------------------------------------------------------
    // The (value, err) pair convention, copied from maki: a missing file is a
    // value a hook checks, not an error that unwinds it.
    let fs = lua.create_table()?;
    fs.set(
        "read",
        lua.create_function(|_, path: String| match std::fs::read_to_string(&path) {
            Ok(text) => Ok((Some(text), None::<String>)),
            Err(err) => Ok((None, Some(err.to_string()))),
        })?,
    )?;
    fs.set(
        "write",
        lua.create_function(|_, (path, content): (String, String)| {
            match std::fs::write(&path, content) {
                Ok(()) => Ok((Some(true), None::<String>)),
                Err(err) => Ok((None, Some(err.to_string()))),
            }
        })?,
    )?;
    fs.set(
        "list",
        lua.create_function(|lua, path: String| {
            let mut names: Vec<String> = match std::fs::read_dir(&path) {
                Ok(entries) => entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect(),
                Err(err) => return Ok((None, Some(err.to_string()))),
            };
            names.sort();
            let table = lua.create_table()?;
            for (i, name) in names.into_iter().enumerate() {
                table.set(i + 1, name)?;
            }
            Ok((Some(table), None::<String>))
        })?,
    )?;
    bough.set("fs", fs)?;

    // ---- bough.exec --------------------------------------------------------
    //
    // NOT A NEW PRIVILEGE: a hook is Lua running in-process as you, and could
    // already read and write any file you can. What this adds is the ability
    // to bridge — the other harnesses' hooks ARE shell commands, and adopting
    // them means running them.
    //
    // It does add a way to BLOCK, which the Luau interrupt cannot cut through:
    // an interrupt cannot preempt a syscall. So the timeout is enforced here,
    // by killing the child, and it is bounded well under the dispatch budget.
    bough.set(
        "exec",
        lua.create_function(|lua, (command, opts): (String, Option<Table>)| {
            let stdin: Option<String> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("stdin").ok().flatten());
            let timeout_ms: u64 = opts
                .as_ref()
                .and_then(|o| o.get::<Option<u64>>("timeout_ms").ok().flatten())
                .unwrap_or(3_000)
                .min(10_000);
            match run_command(&command, stdin.as_deref(), timeout_ms) {
                Ok((code, out, err)) => {
                    let table = lua.create_table()?;
                    table.set("code", code)?;
                    table.set("stdout", out)?;
                    table.set("stderr", err)?;
                    Ok((Some(table), None::<String>))
                }
                Err(message) => Ok((None, Some(message))),
            }
        })?,
    )?;

    lua.globals().set("bough", bough)?;
    Ok(())
}

/// Run one shell command with a hard deadline, returning `(code, out, err)`.
///
/// The child is SIGKILLed on timeout. `sh -c` may leave grandchildren behind,
/// which is the same gap `bash()` has and the same reason the budget here is
/// small: this exists to run someone's `.claude/hooks/*.sh`, not to host work.
fn run_command(
    command: &str,
    stdin: Option<&str>,
    timeout_ms: u64,
) -> Result<(i64, String, String), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start command: {e}"))?;
    let pid = child.id() as i32;
    if let Some(text) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(text.as_bytes());
        }
    } else {
        drop(child.stdin.take());
    }

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
        Ok(Ok(out)) => Ok((
            out.status.code().unwrap_or(-1) as i64,
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => {
            // SAFETY: a plain kill(2) on a process group this call created.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            Err(format!("command exceeded {timeout_ms}ms and was killed"))
        }
    }
}

/// `"TurnEnd"` or `{"TurnStart","TurnEnd"}`.
fn string_list(value: &Value) -> mlua::Result<Vec<String>> {
    match value {
        Value::String(s) => Ok(vec![s.to_str()?.to_string()]),
        Value::Table(t) => {
            let mut out = Vec::new();
            for pair in t.clone().sequence_values::<String>() {
                out.push(pair?);
            }
            Ok(out)
        }
        _ => Err(mlua::Error::runtime(
            "event must be a string or a list of strings",
        )),
    }
}

/// Run every listener for `event` whose pattern matches, newest registration
/// last, and merge what each returned.
fn fire_matching(
    lua: &Lua,
    state: &Shared,
    event: &str,
    pattern: &str,
    data: Value,
    workspace: &str,
) -> HookOutcome {
    // The list is copied out before calling anything: a callback may register
    // or remove autocmds, and iterating the live list while it mutates is how
    // a plugin crashes the host.
    let targets: Vec<(u32, mlua::Function)> = {
        let r = state.borrow();
        r.autocmds
            .iter()
            .filter(|a| a.event == event)
            .filter(|a| match a.pattern.as_deref() {
                None | Some("*") => true,
                Some(p) => p == pattern,
            })
            .filter_map(|a| {
                lua.registry_value::<mlua::Function>(&a.callback)
                    .ok()
                    .map(|f| (a.id, f))
            })
            .collect()
    };
    let mut outcome = HookOutcome::default();
    for (id, callback) in targets {
        // Which file this listener came from, so what it does can be
        // attributed to it. Read before the call: a callback may register or
        // remove listeners, including this one.
        let file = state
            .borrow()
            .autocmds
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.file.clone())
            .unwrap_or_default();
        let before = state.borrow().collected.mark();
        let ev = match event_table(lua, id, event, pattern, data.clone(), workspace) {
            Ok(t) => t,
            Err(err) => {
                outcome.errors.push(err.to_string());
                continue;
            }
        };
        // ONE outcome per callback, folded in after: it is the only way to
        // know which file did what, and attribution is the whole point of the
        // panel's activity column.
        let mut one = HookOutcome::default();
        let mut failed = false;
        match callback.call::<Value>(ev) {
            Ok(returned) => merge_return(lua, &mut one, returned),
            // A hook that throws contributes nothing and says so. It does not
            // stop the turn, and it does not stop the other hooks.
            Err(err) => {
                failed = true;
                one.errors.push(format!("{event} handler failed: {err}"));
            }
        }
        // FIRED COUNTS RUNS, not changes. "used" is the question the panel
        // answers, and a hook that runs every turn and decides to do nothing
        // is being used — it is the one wired to an event that never fires
        // that the user needs to find.
        let mut verbs = one.verbs();
        verbs.extend(state.borrow().collected.since(&before));
        {
            let mut r = state.borrow_mut();
            let entry = r.activity.entry(file).or_default();
            entry.fired += 1;
            entry.last = Some(if failed {
                "failed".to_string()
            } else if verbs.is_empty() {
                "ran".to_string()
            } else {
                verbs.join(", ")
            });
        }
        fold(&mut outcome, one);
        // `once` is consumed even when the callback failed: a listener that
        // throws every time must not fire forever.
        let mut r = state.borrow_mut();
        r.autocmds.retain(|a| !(a.id == id && a.once));
    }
    outcome
}

fn event_table(
    lua: &Lua,
    id: u32,
    event: &str,
    pattern: &str,
    data: Value,
    workspace: &str,
) -> mlua::Result<Table> {
    let ev = lua.create_table()?;
    ev.set("id", id)?;
    ev.set("event", event)?;
    ev.set("match", pattern)?;
    ev.set("data", data)?;
    ev.set("workspace", workspace)?;
    Ok(ev)
}

/// Fold one callback's outcome into the round's.
///
/// LAST WRITER WINS on the single-valued fields — the load order the directory
/// listing fixes is the order the user can see — EXCEPT `stop`, which is
/// sticky: once a hook has stopped the turn, a later one saying nothing must
/// not un-stop it.
fn fold(into: &mut HookOutcome, one: HookOutcome) {
    into.context.extend(one.context);
    if one.decision.is_some() {
        into.decision = one.decision;
        into.reason = one.reason;
    }
    if one.input.is_some() {
        into.input = one.input;
    }
    if one.output.is_some() {
        into.output = one.output;
    }
    if into.stop.is_none() {
        into.stop = one.stop;
    }
    into.effects.extend(one.effects);
    into.errors.extend(one.errors);
}

/// Read the returnable fields off a callback's return value.
fn merge_return(lua: &Lua, outcome: &mut HookOutcome, returned: Value) {
    let Value::Table(t) = returned else {
        return; // returning nothing is the common case
    };
    if let Ok(Some(decision)) = t.get::<Option<String>>("decision") {
        outcome.decision = match decision.as_str() {
            "allow" => Some(ToolDecision::Allow),
            "deny" => Some(ToolDecision::Deny),
            _ => None,
        };
        outcome.reason = t.get::<Option<String>>("reason").ok().flatten();
    }
    if let Ok(Some(input)) = t.get::<Option<Value>>("input") {
        if !matches!(input, Value::Nil) {
            if let Ok(v) = lua.from_value::<serde_json::Value>(input) {
                outcome.input = Some(v);
            }
        }
    }
    if let Ok(Some(output)) = t.get::<Option<String>>("output") {
        outcome.output = Some(output);
    }
    if let Ok(Some(stop)) = t.get::<Option<String>>("stop") {
        if outcome.stop.is_none() {
            outcome.stop = Some(stop);
        }
    }
    if let Ok(Some(context)) = t.get::<Option<String>>("context") {
        outcome.context.push(context);
    }
}

/// One host event, start to finish.
fn run_event(lua: &Lua, state: &Shared, event: &HookEvent, dispatch: &HookDispatch) -> HookOutcome {
    state.borrow_mut().collected = Collected::default();
    let data = lua.to_value(&dispatch.data).unwrap_or(Value::Nil);
    let mut outcome = fire_matching(
        lua,
        state,
        event.name(),
        &dispatch.pattern,
        data,
        &dispatch.workspace,
    );
    // Whatever the callbacks pushed through the imperative verbs joins what
    // they returned. Both channels exist because some things are answers
    // (deny this command) and some are announcements (add this context).
    let collected = std::mem::take(&mut state.borrow_mut().collected);
    outcome.context.extend(collected.context);
    outcome.effects.extend(collected.effects);
    outcome.errors.extend(collected.errors);
    if outcome.stop.is_none() {
        outcome.stop = collected.stop;
    }
    outcome
}

/// Unused today; kept so the map-shaped data a caller builds has one place to
/// be turned into `ev.data`.
#[allow(dead_code)]
pub(super) fn data_of(pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
    serde_json::Value::Object(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect::<serde_json::Map<_, _>>(),
    )
}

/// Reserved for the panel: which events have at least one listener, so the UI
/// can show what is wired without firing anything.
#[allow(dead_code)]
fn listener_counts(state: &Shared) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for a in &state.borrow().autocmds {
        *out.entry(a.event.clone()).or_default() += 1;
    }
    out
}
