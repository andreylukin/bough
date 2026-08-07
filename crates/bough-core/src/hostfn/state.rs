//! `state.get / set / list / delete` (port of `src/hostfn/state.ts`) — the
//! durable notes one line of work keeps for itself across turns.
//!
//! THE INVARIANT THIS HOLDS: **the store is keyed by the LINEAGE ROOT, never by
//! the session id.** A fork, a compaction child and a subagent are one piece of
//! work from the user's point of view, and a note written before a fork has to
//! still be there after it — otherwise the store is useless for exactly the
//! long tasks it exists for, because every branch silently starts empty.
//!
//! [`lineage_root`] is therefore the load-bearing function here, and it is
//! *wider* than `Db::ancestor_chain` on purpose: `ancestor_chain` walks
//! `parentId`, and a subagent has `parentId: null` — walking parents alone
//! would make every subagent its own root. So the walk hops one more edge: at
//! the top of a parent chain that is a `subagent`/`workflow_agent`, it
//! continues from `originId` — the delegation edge — and lands on the
//! spawner's root. Forks and compactions are unaffected.
//!
//! SECOND INVARIANT: **it is notes, not storage.** 16KB per value and 200 keys
//! per lineage, both hard. An oversized value is REJECTED, never truncated — a
//! silently shortened note is a wrong note, and the message says to put the
//! payload in a file and store its path.
//!
//! `hostfn/` imports nothing from the server crate: everything here takes a
//! `Db` or a `TurnCtx`, so the whole module is testable with an in-memory
//! database and no server in sight.

use std::collections::HashSet;

use serde_json::Value;

use crate::errors::{BoughError, ErrorKind};
use crate::harness::protocol::STATE_VERBS;
use crate::schema::parts::SessionKind;
use crate::types::{Clock, Db, TurnCtx};

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Per-value ceiling. Notes, not blobs — read the file yourself for anything bigger.
pub const MAX_VALUE_BYTES: usize = 16_384;

/// Per-lineage ceiling on distinct keys, so a runaway loop cannot fill the database.
pub const MAX_KEYS: usize = 200;

/// Keys are labels, not payloads. A 4KB key is a value in the wrong slot.
pub const MAX_KEY_CHARS: usize = 200;

fn state_error(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::State, message)
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// The session whose store this session shares — the root of its lineage.
///
/// Walks `parentId` to the top (that is `Db::ancestor_chain`), then hops the
/// delegation edge: a `subagent` or `workflow_agent` at the top of a parent
/// chain continues from its `originId`, because it is work its spawner started
/// and shares the store with. The `seen` set is not paranoia about a
/// well-formed tree — it is what stops a cycle introduced by a bad write from
/// hanging every `state.*` call in the process.
///
/// An unknown session is its own root, which is the only answer available and
/// keeps the verbs usable in a fixture that never created a session row.
pub fn lineage_root(db: &dyn Db, session_id: &str) -> Result<String, BoughError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut id = session_id.to_string();
    loop {
        if !seen.insert(id.clone()) {
            return Ok(id);
        }
        let chain = db.ancestor_chain(&id)?;
        let Some(root) = chain.first() else {
            return Ok(id);
        };
        let delegated = matches!(
            root.kind,
            SessionKind::Subagent | SessionKind::WorkflowAgent
        );
        if delegated {
            if let Some(origin) = &root.origin_id {
                if !seen.contains(origin) {
                    id = origin.clone();
                    continue;
                }
            }
        }
        return Ok(root.id.clone());
    }
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

/// The key an argument names. `state.get(key)` sends the bare string and
/// `state.set({key, value})` sends the object, so both shapes are legal input
/// and the difference is the caller's convenience, not a contract.
fn require_key(verb: &str, args: &Value) -> Result<String, BoughError> {
    let key = match args {
        Value::String(s) => Some(s.as_str()),
        Value::Object(o) => o.get("key").and_then(Value::as_str),
        _ => None,
    };
    let Some(key) = key.filter(|k| !k.trim().is_empty()) else {
        return Err(state_error(
            400,
            format!(
                "state.{verb}: a non-empty string key is required — call it as \
                 state.{verb}(\"some-key\")."
            ),
        ));
    };
    let chars = key.chars().count();
    if chars > MAX_KEY_CHARS {
        return Err(state_error(
            400,
            format!(
                "state.{verb}: key too long ({chars} chars, max {MAX_KEY_CHARS}). \
                 Keys are labels; put the long text in the value, or in a file."
            ),
        ));
    }
    Ok(key.to_string())
}

/// One state verb against one lineage's store. `root_id` is resolved by the
/// caller ([`lineage_root`]), so this function is pure with respect to lineage
/// and the clock is injected — the two things that make it testable without a
/// turn.
///
/// Every failure is a `StateError`, which the router renders as a 400 and the
/// program catches as an ordinary exception whose message names the verb, the
/// state that caused it, and the move that resolves it.
pub fn state_verb(
    db: &dyn Db,
    root_id: &str,
    verb: &str,
    args: &Value,
    now: &dyn Fn() -> i64,
) -> Result<Value, BoughError> {
    match verb {
        "get" => {
            let key = require_key("get", args)?;
            // An unset key reads as null rather than throwing: `?? fallback`
            // is the natural idiom and a throw would make every read need a
            // try/catch.
            let Some(raw) = db.get_state(root_id, &key)? else {
                return Ok(Value::Null);
            };
            serde_json::from_str(&raw).map_err(|_| {
                // A row that is not JSON can only come from something outside
                // this module writing the table. Say so rather than crashing
                // the program with a parse error it cannot act on.
                state_error(
                    500,
                    format!(
                        "state.get(\"{key}\"): the stored value is not valid JSON — it was \
                         not written by state.set(). Overwrite it with state.set({{key, \
                         value}}) or remove it with state.delete()."
                    ),
                )
            })
        }

        "set" => {
            let key = require_key("set", args)?;
            // JSON has no `undefined`, so "value is absent from the object"
            // is the arrival shape of both TS cases. Unsetting has its own
            // verb — saying so beats writing the string "undefined".
            let value = match args {
                Value::Object(o) => o.get("value").cloned(),
                _ => None,
            };
            let Some(value) = value else {
                return Err(state_error(
                    400,
                    format!(
                        "state.set(\"{key}\"): a value is required — call it as \
                         state.set({{key, value}}). Use state.delete(\"{key}\") to unset it."
                    ),
                ));
            };
            let serialized = value.to_string();
            let bytes = serialized.len();
            if bytes > MAX_VALUE_BYTES {
                return Err(state_error(
                    400,
                    format!(
                        "state.set(\"{key}\"): value too large ({bytes} bytes, max \
                         {MAX_VALUE_BYTES}) — state holds notes, not payloads. Write the \
                         payload to a file and store its path here instead. Nothing was \
                         stored."
                    ),
                ));
            }
            // Counted only for a key that does not exist yet: overwriting an
            // existing note must keep working at the cap, or a lineage that
            // filled up could not even correct itself.
            if db.get_state(root_id, &key)?.is_none() {
                let used = db.list_state(root_id)?.len();
                if used >= MAX_KEYS {
                    return Err(state_error(
                        400,
                        format!(
                            "state.set(\"{key}\"): too many keys ({used}, max {MAX_KEYS}). \
                             state.list() shows what is stored; state.delete(key) frees a \
                             slot. Nothing was stored."
                        ),
                    ));
                }
            }
            db.set_state(root_id, &key, &serialized, now())?;
            Ok(serde_json::json!({ "ok": true, "key": key, "bytes": bytes }))
        }

        "list" => Ok(serde_json::to_value(db.list_state(root_id)?).unwrap_or(Value::Null)),

        "delete" => {
            let key = require_key("delete", args)?;
            // `removed: false` rather than an error: "there was none" is an
            // answer, and a delete that has to be guarded by a get is two
            // round-trips for nothing.
            let removed = db.delete_state(root_id, &key)?;
            Ok(serde_json::json!({ "ok": true, "key": key, "removed": removed }))
        }

        _ => Err(state_error(
            400,
            format!(
                "state: unknown verb \"{verb}\" — it is one of {}.",
                STATE_VERBS.join(", ")
            ),
        )),
    }
}

// ---------------------------------------------------------------------------
// The bridged host function
// ---------------------------------------------------------------------------

/// Seams, so the verb is drivable with no server, no worker and no real lineage.
#[derive(Clone, Default)]
pub struct StateDeps {
    /// Pin the store's scope. Absent = resolved from the session's lineage per call.
    pub root_id: Option<String>,
    /// Injected clock. Absent = the ctx's.
    pub now: Option<Clock>,
}

/// The bridged `state(verb, argsJson)` for one turn.
///
/// The wire is string-only in both directions, so the worker sends
/// `JSON.stringify(args)` and re-inflates whatever comes back — which is why an
/// unset key must come back as the four characters `null` and not as an empty
/// string.
///
/// The lineage root is resolved per call rather than captured at construction:
/// a turn can outlive a lineage edit, and one `ancestor_chain` walk per
/// `state.*` call is nothing next to the round-trip that carried it.
pub struct StateHostFn {
    ctx: TurnCtx,
    deps: StateDeps,
}

/// Build `state(verb, argsJson)` for one turn.
pub fn create_state_host_fn(ctx: &TurnCtx, deps: StateDeps) -> StateHostFn {
    StateHostFn {
        ctx: ctx.clone(),
        deps,
    }
}

impl StateHostFn {
    pub fn state(&self, verb: &str, args_json: &str) -> Result<String, BoughError> {
        let args = parse_args(verb, args_json)?;
        let now: Clock = self
            .deps
            .now
            .clone()
            .unwrap_or_else(|| self.ctx.app.now.clone());
        let db = self.ctx.app.db.lock().unwrap();
        let root_id = match &self.deps.root_id {
            Some(pinned) => pinned.clone(),
            None => lineage_root(&*db, &self.ctx.session_id)?,
        };
        let result = state_verb(&*db, &root_id, verb, &args, &|| now())?;
        // The wire is `JSON.stringify` of what TS built, and for `list` that
        // is the typed rows in declaration order ({key, bytes, updatedAt}) —
        // a `Value` round-trip would alphabetize the keys.
        if verb == "list" {
            if let Ok(rows) =
                serde_json::from_value::<Vec<crate::types::StateEntry>>(result.clone())
            {
                return Ok(serde_json::to_string(&rows).unwrap_or_else(|_| result.to_string()));
            }
        }
        Ok(result.to_string())
    }

    /// The `HostFns.state` adapter: JSON-string args in protocol order.
    pub fn into_host_fn(self) -> crate::types::HostFn {
        use futures::FutureExt;
        let this = std::sync::Arc::new(self);
        std::sync::Arc::new(move |args: Vec<String>| {
            let this = this.clone();
            async move {
                this.state(
                    args.first().map(String::as_str).unwrap_or_default(),
                    args.get(1).map(String::as_str).unwrap_or_default(),
                )
            }
            .boxed()
        })
    }
}

/// The JSON envelope, parsed at the boundary.
///
/// The program is arbitrary model-written JavaScript, so a malformed argument
/// is a thing that happens; it must become a message the next round can act on
/// rather than a `SyntaxError` with no verb in it.
fn parse_args(verb: &str, args_json: &str) -> Result<Value, BoughError> {
    let text = args_json.trim();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text).map_err(|err| {
        state_error(
            400,
            format!(
                "state.{verb}: the arguments could not be read as JSON ({err}). Pass a \
                 plain value, e.g. state.get(\"key\") or state.set({{key: \"key\", \
                 value: {{…}}}})."
            ),
        )
    })
}

// ---------------------------------------------------------------------------
// tests — ported from `src/hostfn/state.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::bus::Bus;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::Session;
    use crate::turn::queue::TurnRegistry;
    use crate::types::{system_clock, AppCtx, HostState, SharedDb};

    fn mem() -> SqliteDb {
        SqliteDb::new(":memory:", DbOptions::default()).unwrap()
    }

    fn session(id: &str) -> Session {
        session_kind(id, SessionKind::Root, None, None)
    }

    fn session_kind(
        id: &str,
        kind: SessionKind,
        parent_id: Option<&str>,
        origin_id: Option<&str>,
    ) -> Session {
        Session {
            id: id.to_string(),
            title: id.to_string(),
            kind,
            created_at: 1_000,
            parent_id: parent_id.map(str::to_string),
            origin_id: origin_id.map(str::to_string),
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
        }
    }

    /// A frozen clock, so `updatedAt` is a value a test can assert on.
    fn at(t: i64) -> Box<dyn Fn() -> i64> {
        Box::new(move || t)
    }

    fn verb(db: &dyn Db, root: &str, v: &str, args: Value) -> Result<Value, BoughError> {
        state_verb(db, root, v, &args, &|| 5_000)
    }

    fn verb_at(db: &dyn Db, root: &str, v: &str, args: Value, t: i64) -> Value {
        state_verb(db, root, v, &args, &*at(t)).unwrap()
    }

    /// A `TurnCtx` with only the fields `state.*` touches.
    fn turn_ctx(db: SharedDb, session_id: &str, now: Clock) -> TurnCtx {
        let app = AppCtx {
            db,
            bus: Arc::new(Bus::new(system_clock())),
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now,
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(TurnRegistry::new()),
            model_defaults_path: None,
        };
        TurnCtx {
            app,
            session_id: session_id.to_string(),
            turn_id: "t1".to_string(),
            message_id: "m1".to_string(),
            workspace: "/tmp".to_string(),
            model: "test-model".to_string(),
            cancel: CancellationToken::new(),
            exits: Arc::new(Mutex::new(vec![])),
            record: None,
            reads: Arc::new(Mutex::new(vec![])),
            touched: Arc::new(Mutex::new(vec![])),
            mcp_grant: None,
            depth: 0,
        }
    }

    fn frozen(t: i64) -> Clock {
        Arc::new(move || t)
    }

    // ---- the verbs ----------------------------------------------------------

    #[test]
    fn get_set_list_delete_round_trip_any_json() {
        let db = mem();
        assert_eq!(
            verb(&db, "root", "get", json!("todo")).unwrap(),
            Value::Null
        );

        let set = verb_at(
            &db,
            "root",
            "set",
            json!({"key": "todo", "value": {"left": ["a.ts", "b.ts"], "done": 3, "ok": false}}),
            1_000,
        );
        assert_eq!(set["ok"], json!(true));
        assert_eq!(set["key"], json!("todo"));
        assert!(set["bytes"].as_u64().unwrap() > 0);

        assert_eq!(
            verb(&db, "root", "get", json!("todo")).unwrap(),
            json!({"left": ["a.ts", "b.ts"], "done": 3, "ok": false})
        );

        // list gives keys and sizes only — a listing must never drag whole
        // values back into the context this store exists to spare.
        let list = verb(&db, "root", "list", Value::Null).unwrap();
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["key"], json!("todo"));
        assert_eq!(rows[0]["updatedAt"], json!(1_000));
        let mut keys: Vec<&str> = rows[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(keys.join(","), "bytes,key,updatedAt");

        // Deleting twice is not an error: "there was none" is an answer.
        assert_eq!(
            verb(&db, "root", "delete", json!("todo")).unwrap(),
            json!({"ok": true, "key": "todo", "removed": true})
        );
        assert_eq!(
            verb(&db, "root", "delete", json!("todo")).unwrap(),
            json!({"ok": true, "key": "todo", "removed": false})
        );
        assert_eq!(
            verb(&db, "root", "get", json!("todo")).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn an_unset_key_reads_as_null_not_an_error() {
        let db = mem();
        assert_eq!(
            verb(&db, "root", "get", json!("never-written")).unwrap(),
            Value::Null
        );
        // A stored null is indistinguishable from unset, and that is fine:
        // both mean "nothing useful here".
        verb(&db, "root", "set", json!({"key": "k", "value": null})).unwrap();
        assert_eq!(verb(&db, "root", "get", json!("k")).unwrap(), Value::Null);
        // …but the key exists, which `list` shows.
        let list = verb(&db, "root", "list", Value::Null).unwrap();
        assert_eq!(list.as_array().unwrap()[0]["key"], json!("k"));
    }

    #[test]
    fn set_re_set_overwrites_in_place_and_re_stamps_updated_at() {
        let db = mem();
        verb_at(&db, "root", "set", json!({"key": "k", "value": 1}), 1_000);
        verb_at(&db, "root", "set", json!({"key": "k", "value": 2}), 2_000);
        assert_eq!(verb(&db, "root", "get", json!("k")).unwrap(), json!(2));
        let list = verb(&db, "root", "list", Value::Null).unwrap();
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["updatedAt"], json!(2_000));
    }

    #[test]
    fn two_roots_keep_separate_stores() {
        let db = mem();
        verb(&db, "a", "set", json!({"key": "k", "value": 1})).unwrap();
        verb(&db, "b", "set", json!({"key": "k", "value": 2})).unwrap();
        verb(&db, "a", "set", json!({"key": "k", "value": 3})).unwrap();
        assert_eq!(verb(&db, "a", "get", json!("k")).unwrap(), json!(3));
        assert_eq!(verb(&db, "b", "get", json!("k")).unwrap(), json!(2));
        assert_eq!(
            verb(&db, "a", "list", Value::Null)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    // ---- scope — the acceptance criterion -----------------------------------

    #[test]
    fn ac_a_fork_and_its_parent_read_the_same_store() {
        let shared: SharedDb = Arc::new(Mutex::new(mem()));
        {
            let db = shared.lock().unwrap();
            db.create_session(session("root1")).unwrap();
            // A fork is parented at the target's parent, so it shares every
            // ancestor.
            db.create_session(session_kind(
                "fork1",
                SessionKind::Fork,
                Some("root1"),
                Some("root1"),
            ))
            .unwrap();
        }

        let parent = create_state_host_fn(
            &turn_ctx(shared.clone(), "root1", frozen(5_000)),
            StateDeps::default(),
        );
        let fork = create_state_host_fn(
            &turn_ctx(shared.clone(), "fork1", frozen(5_000)),
            StateDeps::default(),
        );

        parent
            .state(
                "set",
                &json!({"key": "ported", "value": ["a.ts"]}).to_string(),
            )
            .unwrap();
        // The fork sees what the parent wrote…
        assert_eq!(fork.state("get", "\"ported\"").unwrap(), "[\"a.ts\"]");
        // …and writing from the fork is visible to the parent. One store, one
        // lineage.
        fork.state(
            "set",
            &json!({"key": "ported", "value": ["a.ts", "b.ts"]}).to_string(),
        )
        .unwrap();
        assert_eq!(
            parent.state("get", "\"ported\"").unwrap(),
            "[\"a.ts\",\"b.ts\"]"
        );

        // Both resolve to the same scope, which is what makes the above true
        // rather than a coincidence of two writes landing in two stores that
        // happen to agree.
        let db = shared.lock().unwrap();
        assert_eq!(lineage_root(&*db, "fork1").unwrap(), "root1");
        assert_eq!(lineage_root(&*db, "root1").unwrap(), "root1");
    }

    #[test]
    fn a_compaction_child_and_a_deep_fork_chain_resolve_to_the_same_root() {
        let db = mem();
        db.create_session(session("root1")).unwrap();
        db.create_session(session_kind(
            "f1",
            SessionKind::Fork,
            Some("root1"),
            Some("root1"),
        ))
        .unwrap();
        db.create_session(session_kind(
            "c1",
            SessionKind::Compaction,
            Some("f1"),
            Some("f1"),
        ))
        .unwrap();
        assert_eq!(lineage_root(&db, "c1").unwrap(), "root1");
    }

    #[test]
    fn a_subagent_shares_its_spawners_store() {
        let shared: SharedDb = Arc::new(Mutex::new(mem()));
        {
            let db = shared.lock().unwrap();
            db.create_session(session("root1")).unwrap();
            // What `agents/subagent` creates: a fresh, task-only thread
            // (`parentId: null`) whose only link upward is the lineage edge.
            db.create_session(session_kind(
                "sub1",
                SessionKind::Subagent,
                None,
                Some("root1"),
            ))
            .unwrap();
            assert_eq!(lineage_root(&*db, "sub1").unwrap(), "root1");
        }

        let spawner = create_state_host_fn(
            &turn_ctx(shared.clone(), "root1", frozen(5_000)),
            StateDeps::default(),
        );
        let child = create_state_host_fn(
            &turn_ctx(shared.clone(), "sub1", frozen(5_000)),
            StateDeps::default(),
        );
        spawner
            .state(
                "set",
                &json!({"key": "plan", "value": "port files 1-40"}).to_string(),
            )
            .unwrap();
        assert_eq!(
            child.state("get", "\"plan\"").unwrap(),
            "\"port files 1-40\""
        );
    }

    #[test]
    fn a_workflow_agent_and_a_subagent_of_a_fork_both_reach_the_lineage_root() {
        let db = mem();
        db.create_session(session("root1")).unwrap();
        db.create_session(session_kind(
            "f1",
            SessionKind::Fork,
            Some("root1"),
            Some("root1"),
        ))
        .unwrap();
        db.create_session(session_kind(
            "sub1",
            SessionKind::Subagent,
            None,
            Some("f1"),
        ))
        .unwrap();
        db.create_session(session_kind(
            "wa1",
            SessionKind::WorkflowAgent,
            None,
            Some("f1"),
        ))
        .unwrap();
        assert_eq!(lineage_root(&db, "sub1").unwrap(), "root1");
        assert_eq!(lineage_root(&db, "wa1").unwrap(), "root1");
    }

    #[test]
    fn lineage_root_survives_a_cycle_and_an_unknown_session() {
        let db = mem();
        // A session whose origin points back at a descendant: a bad write, not
        // a shape the system creates. It must terminate, not hang every state
        // call in the process.
        db.create_session(session_kind("x", SessionKind::Subagent, None, Some("y")))
            .unwrap();
        db.create_session(session_kind("y", SessionKind::Subagent, None, Some("x")))
            .unwrap();
        let root = lineage_root(&db, "x").unwrap();
        assert!(root == "x" || root == "y");
        // An unknown session is its own root — the only answer available.
        assert_eq!(lineage_root(&db, "nobody").unwrap(), "nobody");
    }

    // ---- caps — the other acceptance criterion ------------------------------

    #[test]
    fn ac_a_value_over_16kb_is_rejected_and_nothing_is_stored() {
        let db = mem();
        let oversized = "x".repeat(MAX_VALUE_BYTES); // + 2 quote bytes once serialized
        let err = verb(
            &db,
            "root",
            "set",
            json!({"key": "log", "value": oversized}),
        )
        .unwrap_err();
        assert_eq!(err.status(), 400);
        assert_eq!(err.name(), "StateError");
        assert!(err.to_string().contains("too large"), "{err}");
        assert!(
            err.to_string().contains(&MAX_VALUE_BYTES.to_string()),
            "{err}"
        );
        // The message must say what to do instead, not merely that it failed.
        assert!(err.to_string().contains("file"), "{err}");
        // Rejected, never truncated: a shortened note is a wrong note.
        assert_eq!(verb(&db, "root", "get", json!("log")).unwrap(), Value::Null);
        assert_eq!(verb(&db, "root", "list", Value::Null).unwrap(), json!([]));
    }

    #[test]
    fn the_cap_is_on_bytes_so_a_value_just_under_it_still_lands() {
        let db = mem();
        // JSON adds the two quotes, so this serializes to exactly MAX_VALUE_BYTES.
        let exact = "y".repeat(MAX_VALUE_BYTES - 2);
        let ok = verb(&db, "root", "set", json!({"key": "k", "value": exact})).unwrap();
        assert_eq!(ok["bytes"], json!(MAX_VALUE_BYTES));
        assert_eq!(verb(&db, "root", "get", json!("k")).unwrap(), json!(exact));
        // One more character is one byte too many.
        let over = format!("{exact}y");
        assert!(verb(&db, "root", "set", json!({"key": "k2", "value": over})).is_err());
        // Multi-byte characters count as bytes, not as characters.
        let multi = "é".repeat(MAX_VALUE_BYTES - 1);
        assert!(verb(&db, "root", "set", json!({"key": "k3", "value": multi})).is_err());
    }

    #[test]
    fn the_key_cap_refuses_a_201st_key_but_still_lets_an_existing_one_be_rewritten() {
        let db = mem();
        for i in 0..MAX_KEYS {
            verb(
                &db,
                "root",
                "set",
                json!({"key": format!("k{i}"), "value": i}),
            )
            .unwrap();
        }
        let err = verb(
            &db,
            "root",
            "set",
            json!({"key": "one-too-many", "value": 1}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("too many keys"), "{err}");
        assert!(err.to_string().contains("state.delete"), "{err}");
        // A lineage at the cap must still be able to correct itself, or it is
        // bricked.
        verb(
            &db,
            "root",
            "set",
            json!({"key": "k0", "value": "rewritten"}),
        )
        .unwrap();
        assert_eq!(
            verb(&db, "root", "get", json!("k0")).unwrap(),
            json!("rewritten")
        );
        // …and freeing a slot lets a new key in.
        verb(&db, "root", "delete", json!("k1")).unwrap();
        verb(
            &db,
            "root",
            "set",
            json!({"key": "one-too-many", "value": 1}),
        )
        .unwrap();
        assert_eq!(
            verb(&db, "root", "get", json!("one-too-many")).unwrap(),
            json!(1)
        );
    }

    // ---- argument errors — the text is a product surface --------------------

    #[test]
    fn bad_arguments_name_the_verb_and_the_fix() {
        let db = mem();
        let empty = verb(&db, "root", "get", json!("")).unwrap_err();
        assert_eq!(empty.name(), "StateError");
        assert!(empty.to_string().contains("state.get"), "{empty}");

        assert!(verb(&db, "root", "get", json!({"key": 42})).is_err());
        let long = verb(
            &db,
            "root",
            "set",
            json!({"key": "x".repeat(500), "value": 1}),
        )
        .unwrap_err();
        assert!(long.to_string().contains("key too long"), "{long}");

        let missing = verb(&db, "root", "set", json!({"key": "k"})).unwrap_err();
        assert!(
            missing.to_string().contains("value is required"),
            "{missing}"
        );
        assert!(missing.to_string().contains("state.delete"), "{missing}");

        let unknown = verb(&db, "root", "nope", Value::Null).unwrap_err();
        assert!(unknown.to_string().contains("unknown verb"), "{unknown}");
        assert!(
            unknown.to_string().contains("get, set, list, delete"),
            "{unknown}"
        );
    }

    #[test]
    fn a_row_that_is_not_json_is_reported_not_thrown_raw_at_the_program() {
        let db = mem();
        db.set_state("root", "corrupt", "{not json", 1_000).unwrap();
        let err = verb(&db, "root", "get", json!("corrupt")).unwrap_err();
        assert_eq!(err.status(), 500);
        assert_eq!(err.name(), "StateError");
        assert!(err.to_string().contains("not valid JSON"), "{err}");
    }

    // ---- the bridge ---------------------------------------------------------

    #[test]
    fn the_host_fn_is_string_in_string_out_and_an_unset_key_comes_back_as_null() {
        let shared: SharedDb = Arc::new(Mutex::new(mem()));
        shared
            .lock()
            .unwrap()
            .create_session(session("s1"))
            .unwrap();
        let fns =
            create_state_host_fn(&turn_ctx(shared, "s1", frozen(5_000)), StateDeps::default());

        // The worker sends `JSON.stringify(args)` and re-inflates the reply,
        // so an unset key must be the four characters `null` — an empty string
        // would be a parse error inside the worker, which the program would
        // see as a broken host function.
        assert_eq!(fns.state("get", "\"nope\"").unwrap(), "null");
        assert_eq!(fns.state("list", "null").unwrap(), "[]");

        fns.state("set", &json!({"key": "k", "value": {"a": 1}}).to_string())
            .unwrap();
        assert_eq!(fns.state("get", "\"k\"").unwrap(), "{\"a\":1}");

        // `state.list()` sends no arguments at all in some shapes; an empty
        // string must not be a parse failure.
        assert_eq!(
            fns.state("list", "").unwrap(),
            "[{\"key\":\"k\",\"bytes\":7,\"updatedAt\":5000}]"
        );
    }

    #[test]
    fn the_host_fn_rejects_rather_than_throwing_junk_at_the_program() {
        let shared: SharedDb = Arc::new(Mutex::new(mem()));
        shared
            .lock()
            .unwrap()
            .create_session(session("s1"))
            .unwrap();
        let fns =
            create_state_host_fn(&turn_ctx(shared, "s1", frozen(5_000)), StateDeps::default());
        let bad = fns.state("get", "{not json").unwrap_err();
        assert_eq!(bad.name(), "StateError");
        assert!(
            bad.to_string().contains("could not be read as JSON"),
            "{bad}"
        );
        let unknown = fns.state("frobnicate", "null").unwrap_err();
        assert_eq!(unknown.name(), "StateError");
    }

    #[test]
    fn the_injected_clock_is_used_and_root_id_can_be_pinned() {
        let shared: SharedDb = Arc::new(Mutex::new(mem()));
        let fns = create_state_host_fn(
            &turn_ctx(shared.clone(), "whatever", frozen(0)),
            StateDeps {
                root_id: Some("pinned".to_string()),
                now: Some(frozen(777)),
            },
        );
        fns.state("set", &json!({"key": "k", "value": 1}).to_string())
            .unwrap();
        let db = shared.lock().unwrap();
        assert_eq!(verb(&*db, "pinned", "get", json!("k")).unwrap(), json!(1));
        let list = verb(&*db, "pinned", "list", Value::Null).unwrap();
        assert_eq!(list.as_array().unwrap()[0]["updatedAt"], json!(777));
    }
}
