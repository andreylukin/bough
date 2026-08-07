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
        match std::fs::read_to_string(&file) {
            Ok(src) => match lua.load(&src).set_name(file.to_string_lossy()).exec() {
                Ok(()) => loaded.push(file),
                Err(err) => failed.push((file, err.to_string())),
            },
            Err(err) => failed.push((file, err.to_string())),
        }
    }
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

#[derive(Default)]
struct Registry {
    autocmds: Vec<Autocmd>,
    next_id: u32,
    collected: Collected,
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
                r.autocmds.push(Autocmd {
                    id,
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
                let _ = fire_matching(lua, &reg, &name, &pattern, data.clone());
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
    bough.set("fs", fs)?;

    lua.globals().set("bough", bough)?;
    Ok(())
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
        let ev = match event_table(lua, id, event, pattern, data.clone()) {
            Ok(t) => t,
            Err(err) => {
                outcome.errors.push(err.to_string());
                continue;
            }
        };
        match callback.call::<Value>(ev) {
            Ok(returned) => merge_return(lua, &mut outcome, returned),
            // A hook that throws contributes nothing and says so. It does not
            // stop the turn, and it does not stop the other hooks.
            Err(err) => outcome
                .errors
                .push(format!("{event} handler failed: {err}")),
        }
        // `once` is consumed even when the callback failed: a listener that
        // throws every time must not fire forever.
        let mut r = state.borrow_mut();
        r.autocmds.retain(|a| !(a.id == id && a.once));
    }
    outcome
}

fn event_table(lua: &Lua, id: u32, event: &str, pattern: &str, data: Value) -> mlua::Result<Table> {
    let ev = lua.create_table()?;
    ev.set("id", id)?;
    ev.set("event", event)?;
    ev.set("match", pattern)?;
    ev.set("data", data)?;
    Ok(ev)
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
    let mut outcome = fire_matching(lua, state, event.name(), &dispatch.pattern, data);
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
