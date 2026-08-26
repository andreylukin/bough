//! Invariant: the ward's own vocabulary is a MAP IN AND A LIST OUT. Everything crossing the rhai
//! boundary is converted here, so a ward can never hold a handle to anything of the harness's:
//! what goes in is data that was already fetched, and what comes back is data that still has to be
//! parsed into [`RuntimeAction`]s before anything happens.

use bough_plugin_runtime_actions::RuntimeAction;
use rhai::{Dynamic, Map};

use crate::{WardError, WardEvent, WardView};

/// The prelude every ward is compiled with: `cx.recent(kind, n)` and `cx.already(ref)` are the two
/// reads the contract promises, and they read PRE-FETCHED arrays on `cx` — so the script performs
/// no I/O to use them. Compiled as its own AST and merged, so a ward's own line numbers survive.
pub const PRELUDE: &str = r#"
fn recent(cx, kind, n) {
    let out = [];
    for e in cx.recent {
        if e.kind == kind && out.len() < n { out.push(e); }
    }
    out
}
fn already(cx, r) {
    for a in cx.acted { if a == r { return true; } }
    false
}
"#;

/// One ledger step as the script sees it.
pub fn event_map(ev: &WardEvent) -> Map {
    let mut m = Map::new();
    m.insert("kind".into(), ev.kind.as_str().into());
    m.insert("seq".into(), Dynamic::from(ev.seq.0 as i64));
    m.insert("traj".into(), ev.traj.as_str().into());
    m.insert(
        "agent".into(),
        match &ev.agent {
            Some(a) => a.as_str().into(),
            None => Dynamic::UNIT,
        },
    );
    m.insert("wake".into(), ev.wake.as_str().into());
    m.insert("at_ms".into(), Dynamic::from(ev.at.timestamp_millis()));
    m.insert("body".into(), json_to_dynamic(&ev.body));
    m.insert(
        "cites".into(),
        Dynamic::from(
            ev.cites
                .iter()
                .map(|c| Dynamic::from(c.to_string()))
                .collect::<rhai::Array>(),
        ),
    );
    m.insert(
        "refs".into(),
        Dynamic::from(
            ev.refs
                .iter()
                .map(|r| Dynamic::from(r.to_string()))
                .collect::<rhai::Array>(),
        ),
    );
    m
}

/// The read-only context as the script sees it.
pub fn view_map(cx: &WardView) -> Map {
    let mut m = Map::new();
    m.insert("ward".into(), cx.ward.clone().into());
    m.insert(
        "agent_names".into(),
        Dynamic::from(
            cx.agent_names
                .iter()
                .map(|n| Dynamic::from(n.clone()))
                .collect::<rhai::Array>(),
        ),
    );
    m.insert("now_ms".into(), Dynamic::from(cx.now_ms));
    m.insert(
        "recent".into(),
        Dynamic::from(
            cx.recent
                .iter()
                .map(|e| Dynamic::from(event_map(e)))
                .collect::<rhai::Array>(),
        ),
    );
    m.insert(
        "acted".into(),
        Dynamic::from(
            cx.acted
                .iter()
                .map(|r| Dynamic::from(r.to_string()))
                .collect::<rhai::Array>(),
        ),
    );
    m
}

/// rhai → json. Total: anything a ward can build becomes a `Value` or a named failure.
pub fn dynamic_to_json(d: &Dynamic) -> Result<serde_json::Value, String> {
    if d.is_unit() {
        return Ok(serde_json::Value::Null);
    }
    if let Ok(b) = d.as_bool() {
        return Ok(serde_json::Value::Bool(b));
    }
    if let Ok(i) = d.as_int() {
        return Ok(serde_json::Value::Number(i.into()));
    }
    if let Ok(f) = d.as_float() {
        return Ok(serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null));
    }
    if let Some(s) = d.clone().try_cast::<rhai::ImmutableString>() {
        return Ok(serde_json::Value::String(s.to_string()));
    }
    if let Some(a) = d.clone().try_cast::<rhai::Array>() {
        return Ok(serde_json::Value::Array(
            a.iter().map(dynamic_to_json).collect::<Result<_, _>>()?,
        ));
    }
    if let Some(m) = d.clone().try_cast::<Map>() {
        let mut out = serde_json::Map::new();
        for (k, v) in m.iter() {
            out.insert(k.to_string(), dynamic_to_json(v)?);
        }
        return Ok(serde_json::Value::Object(out));
    }
    Err(format!(
        "a `{}` cannot cross the ward boundary",
        d.type_name()
    ))
}

/// json → rhai.
pub fn json_to_dynamic(v: &serde_json::Value) -> Dynamic {
    match v {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => Dynamic::from(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Dynamic::from(i),
            None => Dynamic::from(n.as_f64().unwrap_or_default()),
        },
        serde_json::Value::String(s) => Dynamic::from(s.clone()),
        serde_json::Value::Array(a) => {
            Dynamic::from(a.iter().map(json_to_dynamic).collect::<rhai::Array>())
        }
        serde_json::Value::Object(o) => {
            let mut m = Map::new();
            for (k, val) in o {
                m.insert(k.as_str().into(), json_to_dynamic(val));
            }
            Dynamic::from(m)
        }
    }
}

/// What the script returned, parsed. A non-array, or an element that is not one of the six kinds,
/// is a NAMED failure of that ward — never a silently dropped action.
pub fn actions_of(ward: &str, returned: &Dynamic) -> Result<Vec<RuntimeAction>, WardError> {
    let json = dynamic_to_json(returned).map_err(|detail| WardError::BadReturn {
        ward: ward.to_string(),
        detail,
    })?;
    let arr = json.as_array().ok_or_else(|| WardError::BadReturn {
        ward: ward.to_string(),
        detail: format!("expected a list of actions, got {json}"),
    })?;
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        out.push(
            serde_json::from_value::<RuntimeAction>(item.clone()).map_err(|e| {
                WardError::BadReturn {
                    ward: ward.to_string(),
                    detail: format!("action {i}: {e}"),
                }
            })?,
        );
    }
    Ok(out)
}
