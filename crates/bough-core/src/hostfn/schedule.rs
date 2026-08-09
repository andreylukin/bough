//! Schedule spec grammar constants + validated CRUD (port of
//! `src/hostfn/schedule.ts`), shared by the REST routes and the `schedule.*`
//! host fn — one validated path, deliberately: a spec that parses over HTTP
//! but not from a program (or the reverse) is a bug nobody finds until a
//! schedule silently never fires.
//!
//! THE INVARIANT (stated in `crate::schedules`, enforced here): **`next_run_at`
//! is always computed FROM NOW, never from the stale stored value.** It shows
//! up twice more in [`schedule_patch`]: changing the spec recomputes from now
//! (the old cadence's next slot means nothing under a new cadence), and
//! re-enabling recomputes from now — otherwise the disabled stretch reads as
//! downtime and the schedule fires the instant it is switched back on, which
//! is not what "enable" means to anybody.
//!
//! `sessionId` is stamped by the host fn from the calling turn, NEVER taken
//! from the wire — a program must not point another conversation's wake at
//! itself. The REST path leaves it null: the firing reports to nobody.
//!
//! The pure grammar half ([`crate::schedules::parse_spec`] /
//! [`crate::schedules::next_run`]) landed with wave 1 in `schedules.rs`; this
//! file holds the constants it shares plus the CRUD and the bridged verb.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::errors::{BoughError, ErrorKind};
use crate::schedules::next_run;
use crate::schema::parts::Schedule;
use crate::schema::requests::{CreateScheduleBody, PatchScheduleBody};
use crate::types::{Clock, Db, HostFn, Patch, TurnCtx};

/// The exact string error messages embed; a REST test asserts
/// `every:<N><m|h|d>` appears in the 400 body.
pub const SPEC_HELP: &str = "every:<N><m|h|d> with N \u{2265} 1 (every:30m, every:2h, every:1d) or daily@HH:MM in local wall-clock time (daily@09:00)";

/// A parsed schedule spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedSpec {
    Every { ms: i64 },
    Daily { hh: u8, mm: u8 },
}

fn schedule_error(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Schedule, message)
}

// ---------------------------------------------------------------------------
// Workspace resolution
// ---------------------------------------------------------------------------

/// Resolve and validate a workspace path, or fail with a 400 naming the
/// problem. Injected so the CRUD is testable without a real directory; the
/// default below is the production one.
///
/// NOTE: this restates the session-create workspace rule rather than importing
/// it, deliberately (`hostfn` must not reference the server crate): a schedule
/// pointed at a path that does not exist would surface a year of shell
/// failures inside every fired session, and read as the agent being broken
/// rather than the schedule being wrong.
pub type WorkspaceResolver = Arc<dyn Fn(&str) -> Result<String, BoughError> + Send + Sync>;

/// Expand `~`, make absolute, require it to be a directory that exists now.
pub fn resolve_workspace(raw: &str) -> Result<String, BoughError> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let trimmed = raw.trim();
    let expanded: PathBuf = if trimmed == "~" {
        home
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(trimmed)
    };
    let abs = if expanded.is_absolute() {
        lexical_clean(&expanded)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        lexical_clean(&cwd.join(&expanded))
    };
    let stat = std::fs::metadata(&abs).map_err(|_| {
        schedule_error(
            400,
            format!(
                "workspace does not exist: {}. Point the schedule at a checkout that is \
                 there now — every firing opens a session in it.",
                abs.display()
            ),
        )
    })?;
    if !stat.is_dir() {
        return Err(schedule_error(
            400,
            format!("workspace is not a directory: {}", abs.display()),
        ));
    }
    Ok(abs.to_string_lossy().into_owned())
}

/// `path.resolve`-style lexical normalization (no symlink following, no stat):
/// `confine` against the filesystem root gives exactly that walk, and every
/// absolute path is inside `/`.
fn lexical_clean(p: &Path) -> PathBuf {
    crate::paths::confine(Path::new("/"), p).unwrap_or_else(|_| p.to_path_buf())
}

/// The seams the CRUD takes. All default to production behavior.
#[derive(Clone, Default)]
pub struct ScheduleDeps {
    /// Injected clock. Absent = the system clock.
    pub now: Option<Clock>,
    /// Absent = [`resolve_workspace`].
    pub workspace: Option<WorkspaceResolver>,
    /// The conversation the schedule reports its firings back to
    /// (`crate::schedules`). Stamped by the host fn from the calling turn,
    /// NEVER taken from the wire. Absent (the REST path) = the firing reports
    /// to nobody.
    pub session_id: Option<String>,
}

impl ScheduleDeps {
    fn now_ms(&self) -> i64 {
        match &self.now {
            Some(clock) => clock(),
            None => crate::types::system_clock()(),
        }
    }
    fn resolve(&self, raw: &str) -> Result<String, BoughError> {
        match &self.workspace {
            Some(resolver) => resolver(raw),
            None => resolve_workspace(raw),
        }
    }
}

// ---------------------------------------------------------------------------
// Validated CRUD — shared by the REST routes and the host fn
// ---------------------------------------------------------------------------

fn require_spec(spec: &str) -> Result<(), BoughError> {
    if crate::schedules::parse_spec(spec).is_some() {
        return Ok(());
    }
    Err(schedule_error(
        400,
        format!(
            "invalid spec {} — use {SPEC_HELP}",
            serde_json::to_string(spec).unwrap_or_else(|_| format!("{spec:?}"))
        ),
    ))
}

fn require_schedule(db: &dyn Db, id: &str) -> Result<Schedule, BoughError> {
    db.get_schedule(id)?.ok_or_else(|| {
        schedule_error(
            404,
            format!("schedule {id} not found — schedule.list() returns the ids that exist"),
        )
    })
}

/// Create a schedule. The first `next_run_at` is computed from `now`, like
/// every other one: a schedule created at 09:00 with `every:2h` is next due at
/// 11:00, not immediately.
pub fn schedule_create(
    db: &dyn Db,
    body: &CreateScheduleBody,
    deps: &ScheduleDeps,
) -> Result<Schedule, BoughError> {
    let now = deps.now_ms();
    require_spec(&body.spec)?;
    let workspace = match &body.workspace {
        Some(w) if !w.is_empty() => Some(deps.resolve(w)?),
        _ => None,
    };
    db.create_schedule(Schedule {
        id: uuid::Uuid::new_v4().to_string(),
        title: body.title.clone(),
        prompt: body.prompt.clone(),
        workspace,
        spec: body.spec.clone(),
        enabled: body.enabled.unwrap_or(true),
        created_at: now,
        last_run_at: None,
        next_run_at: next_run(&body.spec, now)?,
        session_id: deps.session_id.clone(),
    })
}

/// Patch a schedule. `next_run_at` is recomputed from now in EXACTLY two
/// cases, and both are the invariant seen from a different angle: the spec
/// changed, or the schedule went disabled → enabled.
pub fn schedule_patch(
    db: &dyn Db,
    id: &str,
    patch: &PatchScheduleBody,
    deps: &ScheduleDeps,
) -> Result<Schedule, BoughError> {
    let now = deps.now_ms();
    let current = require_schedule(db, id)?;
    if let Some(spec) = &patch.spec {
        require_spec(spec)?;
    }

    let workspace = match &patch.workspace {
        Patch::Keep => current.workspace.clone(),
        Patch::Clear => None,
        Patch::Set(w) => Some(deps.resolve(w)?),
    };

    let mut next = Schedule {
        title: patch.title.clone().unwrap_or_else(|| current.title.clone()),
        prompt: patch
            .prompt
            .clone()
            .unwrap_or_else(|| current.prompt.clone()),
        workspace,
        spec: patch.spec.clone().unwrap_or_else(|| current.spec.clone()),
        enabled: patch.enabled.unwrap_or(current.enabled),
        ..current.clone()
    };
    if next.spec != current.spec || (next.enabled && !current.enabled) {
        next.next_run_at = next_run(&next.spec, now)?;
    }
    db.update_schedule(&next)?;
    require_schedule(db, id)
}

/// Delete a schedule. 404s rather than silently succeeding on an unknown id.
pub fn schedule_remove(db: &dyn Db, id: &str) -> Result<(), BoughError> {
    require_schedule(db, id)?;
    db.delete_schedule(id)
}

// ---------------------------------------------------------------------------
// The host function
// ---------------------------------------------------------------------------

/// The `schedule.*` verbs, over the same validated CRUD the routes use.
///
/// `default_workspace` fills in when `add` omits one, so "check the deploy
/// each morning" schedules against the checkout the conversation is already
/// about rather than against the server's cwd.
pub fn schedule_verb(
    db: &dyn Db,
    verb: &str,
    args: &Value,
    default_workspace: Option<&str>,
    deps: &ScheduleDeps,
) -> Result<Value, BoughError> {
    match verb {
        "list" => Ok(serde_json::to_value(db.list_schedules()?).unwrap_or(Value::Null)),
        "add" => {
            let mut body = parse_add_body(args).map_err(|issues| {
                schedule_error(
                    400,
                    format!(
                        "schedule.add: {issues}. It takes \
                         {{title, prompt, spec, workspace?, enabled?}} — spec is {SPEC_HELP}.",
                    ),
                )
            })?;
            // An explicit workspace wins; an absent one defaults to the caller's.
            if body.workspace.as_deref().unwrap_or("").is_empty() {
                if let Some(default) = default_workspace {
                    body.workspace = Some(default.to_string());
                }
            }
            let created = schedule_create(db, &body, deps)?;
            Ok(serde_json::to_value(created).unwrap_or(Value::Null))
        }
        "enable" | "disable" => {
            let id = schedule_id(verb, args)?;
            let patch = PatchScheduleBody {
                enabled: Some(verb == "enable"),
                ..Default::default()
            };
            let patched = schedule_patch(db, &id, &patch, deps)?;
            Ok(serde_json::to_value(patched).unwrap_or(Value::Null))
        }
        "remove" => {
            let id = schedule_id(verb, args)?;
            schedule_remove(db, &id)?;
            Ok(serde_json::json!({ "ok": true, "removed": id }))
        }
        _ => Err(schedule_error(
            400,
            format!(
                "unknown schedule verb: {verb}. The verbs are list, add, enable, disable, remove."
            ),
        )),
    }
}

/// The hand-rolled `CreateScheduleBody` validation for `schedule.add` — the
/// Zod `safeParse` of the port, producing one line of `path: problem` issues
/// (the wrapper text around them is verbatim product surface).
fn parse_add_body(args: &Value) -> Result<CreateScheduleBody, String> {
    let Some(obj) = args.as_object() else {
        return Err("(root): expected an object".to_string());
    };
    let mut issues: Vec<String> = Vec::new();
    let mut string_field = |name: &str, required: bool| -> Option<String> {
        match obj.get(name) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::String(_)) => {
                issues.push(format!("{name}: must not be empty"));
                None
            }
            Some(_) => {
                issues.push(format!("{name}: expected a string"));
                None
            }
            None => {
                if required {
                    issues.push(format!("{name}: required"));
                }
                None
            }
        }
    };
    let title = string_field("title", true);
    let prompt = string_field("prompt", true);
    let spec = string_field("spec", true);
    let workspace = string_field("workspace", false);
    let enabled = match obj.get("enabled") {
        None => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            issues.push("enabled: expected a boolean".to_string());
            None
        }
    };
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }
    Ok(CreateScheduleBody {
        title: title.unwrap_or_default(),
        prompt: prompt.unwrap_or_default(),
        workspace,
        spec: spec.unwrap_or_default(),
        enabled,
    })
}

/// The single-argument verbs all take a bare id string. Say so when they do not.
fn schedule_id(verb: &str, args: &Value) -> Result<String, BoughError> {
    if let Some(s) = args.as_str() {
        if !s.trim().is_empty() {
            return Ok(s.to_string());
        }
    }
    Err(schedule_error(
        400,
        format!(
            "schedule.{verb}: pass the schedule id as a string — schedule.{verb}(\"<id>\"). \
             schedule.list() returns the ids.",
        ),
    ))
}

/// Build the bridged `schedule` host function for one turn.
///
/// The wire is string-in/string-out (`harness/protocol`), so the verb's
/// argument arrives as JSON and the result goes back as JSON; the worker
/// rebuilds the `schedule.add(...)` method object the program actually calls.
pub fn create_schedule_host_fn(ctx: &TurnCtx, deps: ScheduleDeps) -> HostFn {
    let db = ctx.app.db.clone();
    // The session's own checkout is the default. `TurnCtx.workspace` is
    // already resolved for this turn, so a schedule created from a program
    // always names a real directory instead of inheriting whatever the
    // server's cwd happens to be at fire time, months later.
    let default_workspace = if ctx.workspace.is_empty() {
        None
    } else {
        Some(ctx.workspace.clone())
    };
    let call_deps = ScheduleDeps {
        now: deps.now.clone().or_else(|| Some(ctx.app.now.clone())),
        workspace: deps.workspace.clone(),
        // The calling conversation is where the firings report back — see
        // `ScheduleDeps::session_id` for why this is stamped here and only here.
        session_id: Some(ctx.session_id.clone()),
    };
    Arc::new(move |args: Vec<String>| {
        let db = db.clone();
        let deps = call_deps.clone();
        let default_workspace = default_workspace.clone();
        let verb = args.first().cloned().unwrap_or_default();
        let args_json = args.get(1).cloned().unwrap_or_default();
        Box::pin(async move {
            let parsed: Value = if args_json.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&args_json).map_err(|_| {
                    schedule_error(
                        400,
                        format!("schedule.{verb}: arguments were not valid JSON"),
                    )
                })?
            };
            let result = {
                let guard = db
                    .lock()
                    .map_err(|_| schedule_error(500, "schedule: the database lock is poisoned"))?;
                schedule_verb(&*guard, &verb, &parsed, default_workspace.as_deref(), &deps)?
            };
            Ok(serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string()))
        })
    })
}

// ---------------------------------------------------------------------------
// Tests — port of src/hostfn/schedule.test.ts (grammar tests live with the
// pure math in `crate::schedules`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use chrono::TimeZone;

    /// `Date.UTC(2026, 0, 15, 12, 0, 0)`.
    fn t0() -> i64 {
        chrono::Utc
            .with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
            .unwrap()
            .timestamp_millis()
    }

    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 3_600_000;

    fn db() -> SqliteDb {
        SqliteDb::new(":memory:", DbOptions::default()).unwrap()
    }

    /// A workspace resolver that accepts anything — no directory is touched.
    fn any_workspace() -> WorkspaceResolver {
        Arc::new(|raw: &str| Ok(format!("/resolved{raw}")))
    }

    fn deps_at(at: i64) -> ScheduleDeps {
        ScheduleDeps {
            now: Some(Arc::new(move || at)),
            workspace: Some(any_workspace()),
            session_id: None,
        }
    }

    fn create_body(spec: &str) -> CreateScheduleBody {
        CreateScheduleBody {
            title: "nightly".into(),
            prompt: "check the deploy".into(),
            workspace: None,
            spec: spec.into(),
            enabled: None,
        }
    }

    fn seed(store: &SqliteDb) -> Schedule {
        schedule_create(store, &create_body("every:30m"), &deps_at(t0())).unwrap()
    }

    fn assert_schedule_err(err: BoughError, status: u16, fragment: &str) {
        assert_eq!(err.name(), "ScheduleError", "{err}");
        assert_eq!(err.status(), status, "{err}");
        assert!(
            err.to_string().contains(fragment),
            "expected {fragment:?} in: {err}"
        );
    }

    // ---- CRUD ---------------------------------------------------------------

    #[test]
    fn schedule_create_stores_the_row_with_next_run_at_one_interval_out() {
        let store = db();
        let created = schedule_create(&store, &create_body("every:2h"), &deps_at(t0())).unwrap();
        assert!(created.enabled);
        assert_eq!(created.workspace, None);
        assert_eq!(created.last_run_at, None);
        assert_eq!(created.created_at, t0());
        assert_eq!(created.next_run_at, t0() + 2 * HOUR);
        assert_eq!(
            store
                .list_schedules()
                .unwrap()
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
            vec![created.id]
        );
    }

    #[test]
    fn schedule_create_rejects_a_bad_spec_with_the_grammar() {
        let store = db();
        let err = schedule_create(&store, &create_body("hourly"), &deps_at(t0())).unwrap_err();
        assert_schedule_err(err, 400, "every:<N><m|h|d>");
        assert!(store.list_schedules().unwrap().is_empty());
    }

    #[test]
    fn schedule_create_resolves_the_workspace_through_the_injected_resolver() {
        let store = db();
        let body = CreateScheduleBody {
            workspace: Some("~/repo".into()),
            ..create_body("every:1d")
        };
        let created = schedule_create(&store, &body, &deps_at(t0())).unwrap();
        assert_eq!(created.workspace.as_deref(), Some("/resolved~/repo"));
    }

    #[test]
    fn schedule_create_surfaces_a_workspace_that_does_not_exist() {
        let store = db();
        let deps = ScheduleDeps {
            now: Some(Arc::new(t0)),
            workspace: Some(Arc::new(|_raw: &str| {
                Err(BoughError::http(
                    400,
                    ErrorKind::Schedule,
                    "workspace does not exist: /nope",
                ))
            })),
            session_id: None,
        };
        let body = CreateScheduleBody {
            workspace: Some("/nope".into()),
            ..create_body("every:1d")
        };
        let err = schedule_create(&store, &body, &deps).unwrap_err();
        assert_schedule_err(err, 400, "workspace does not exist");
    }

    #[test]
    fn the_production_resolver_requires_an_existing_directory() {
        let err = resolve_workspace("/definitely/not/a/real/dir").unwrap_err();
        assert_schedule_err(
            err,
            400,
            "workspace does not exist: /definitely/not/a/real/dir. Point the schedule at a checkout",
        );
        // A real directory resolves to itself, absolutized.
        let dir = std::env::temp_dir();
        let resolved = resolve_workspace(dir.to_str().unwrap()).unwrap();
        assert!(std::path::Path::new(&resolved).is_absolute());
    }

    #[test]
    fn schedule_patch_leaves_next_run_at_alone_for_a_cosmetic_edit() {
        let store = db();
        let created = seed(&store);
        let patch = PatchScheduleBody {
            title: Some("renamed".into()),
            ..Default::default()
        };
        let patched =
            schedule_patch(&store, &created.id, &patch, &deps_at(t0() + 5 * MINUTE)).unwrap();
        assert_eq!(patched.title, "renamed");
        assert_eq!(patched.next_run_at, created.next_run_at);
    }

    #[test]
    fn schedule_patch_recomputes_next_run_at_from_now_when_the_spec_changes() {
        let store = db();
        let created = seed(&store);
        let at = t0() + 5 * MINUTE;
        let patch = PatchScheduleBody {
            spec: Some("every:2h".into()),
            ..Default::default()
        };
        let patched = schedule_patch(&store, &created.id, &patch, &deps_at(at)).unwrap();
        assert_eq!(patched.next_run_at, at + 2 * HOUR);
    }

    #[test]
    fn re_enabling_recomputes_from_now_the_disabled_stretch_is_not_downtime() {
        let store = db();
        let created = seed(&store);
        let disable = PatchScheduleBody {
            enabled: Some(false),
            ..Default::default()
        };
        schedule_patch(&store, &created.id, &disable, &deps_at(t0())).unwrap();

        // A week later. If re-enabling kept the stale next_run_at, the
        // schedule would be due the instant it was switched back on — which is
        // not what "enable" means.
        let at = t0() + 7 * 24 * HOUR;
        let enable = PatchScheduleBody {
            enabled: Some(true),
            ..Default::default()
        };
        let reenabled = schedule_patch(&store, &created.id, &enable, &deps_at(at)).unwrap();
        assert!(reenabled.enabled);
        assert_eq!(reenabled.next_run_at, at + 30 * MINUTE);
        assert!(store.due_schedules(at).unwrap().is_empty());
    }

    #[test]
    fn disabling_does_not_recompute_and_a_disabled_row_is_never_due() {
        let store = db();
        let created = seed(&store);
        let patch = PatchScheduleBody {
            enabled: Some(false),
            ..Default::default()
        };
        let patched = schedule_patch(&store, &created.id, &patch, &deps_at(t0() + MINUTE)).unwrap();
        assert_eq!(patched.next_run_at, created.next_run_at);
        assert!(store.due_schedules(t0() + 10 * HOUR).unwrap().is_empty());
    }

    #[test]
    fn schedule_patch_clears_the_workspace_with_an_explicit_null() {
        let store = db();
        let body = CreateScheduleBody {
            workspace: Some("/repo".into()),
            ..create_body("every:1d")
        };
        let created = schedule_create(&store, &body, &deps_at(t0())).unwrap();
        assert!(created.workspace.is_some());
        let patch = PatchScheduleBody {
            workspace: Patch::Clear,
            ..Default::default()
        };
        let patched = schedule_patch(&store, &created.id, &patch, &deps_at(t0())).unwrap();
        assert_eq!(patched.workspace, None);
    }

    #[test]
    fn patching_and_removing_an_unknown_id_is_a_404_not_a_silent_success() {
        let store = db();
        let patch = PatchScheduleBody {
            title: Some("x".into()),
            ..Default::default()
        };
        assert_schedule_err(
            schedule_patch(&store, "nope", &patch, &deps_at(t0())).unwrap_err(),
            404,
            "not found",
        );
        assert_schedule_err(
            schedule_remove(&store, "nope").unwrap_err(),
            404,
            "not found",
        );
    }

    #[test]
    fn schedule_remove_deletes_the_row() {
        let store = db();
        let created = seed(&store);
        schedule_remove(&store, &created.id).unwrap();
        assert!(store.get_schedule(&created.id).unwrap().is_none());
    }

    // ---- the verbs ----------------------------------------------------------

    #[test]
    fn schedule_verb_add_defaults_the_workspace_to_the_callers() {
        let store = db();
        let added = schedule_verb(
            &store,
            "add",
            &serde_json::json!({"title": "t", "prompt": "p", "spec": "every:1h"}),
            Some("/work/repo"),
            &deps_at(t0()),
        )
        .unwrap();
        assert_eq!(added["workspace"], "/resolved/work/repo");
    }

    #[test]
    fn schedule_verb_add_keeps_an_explicit_workspace_over_the_default() {
        let store = db();
        let added = schedule_verb(
            &store,
            "add",
            &serde_json::json!({
                "title": "t", "prompt": "p", "spec": "every:1h", "workspace": "/elsewhere"
            }),
            Some("/work/repo"),
            &deps_at(t0()),
        )
        .unwrap();
        assert_eq!(added["workspace"], "/resolved/elsewhere");
    }

    #[test]
    fn schedule_verb_add_reports_a_malformed_argument_object() {
        let store = db();
        let err = schedule_verb(
            &store,
            "add",
            &serde_json::json!({"title": "t"}),
            None,
            &deps_at(t0()),
        )
        .unwrap_err();
        assert_schedule_err(err, 400, "schedule.add");
    }

    #[test]
    fn schedule_verb_enable_disable_remove_take_a_bare_id_string() {
        let store = db();
        let created = seed(&store);

        let disabled = schedule_verb(
            &store,
            "disable",
            &Value::String(created.id.clone()),
            None,
            &deps_at(t0()),
        )
        .unwrap();
        assert_eq!(disabled["enabled"], false);

        let enabled = schedule_verb(
            &store,
            "enable",
            &Value::String(created.id.clone()),
            None,
            &deps_at(t0() + HOUR),
        )
        .unwrap();
        assert_eq!(enabled["enabled"], true);

        let removed = schedule_verb(
            &store,
            "remove",
            &Value::String(created.id.clone()),
            None,
            &deps_at(t0()),
        )
        .unwrap();
        assert_eq!(
            removed,
            serde_json::json!({"ok": true, "removed": created.id})
        );
    }

    #[test]
    fn schedule_verb_says_how_to_call_a_verb_that_got_the_wrong_argument() {
        let store = db();
        let err = schedule_verb(
            &store,
            "enable",
            &serde_json::json!({"id": "x"}),
            None,
            &deps_at(t0()),
        )
        .unwrap_err();
        assert_schedule_err(err, 400, "as a string");
    }

    #[test]
    fn schedule_verb_names_the_verbs_when_it_gets_an_unknown_one() {
        let store = db();
        let err = schedule_verb(&store, "pause", &Value::Null, None, &deps_at(t0())).unwrap_err();
        assert_schedule_err(err, 400, "list, add, enable");
    }

    // ---- the bridged host function ------------------------------------------

    fn turn_ctx(db: crate::types::SharedDb, workspace: &str) -> TurnCtx {
        use std::sync::Mutex;
        let app = crate::types::AppCtx {
            db,
            bus: Arc::new(crate::bus::Bus::new(Arc::new(t0))),
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: Arc::new(t0),
            cheap: None,
            host: Arc::new(crate::types::HostState::new()),
            starter: Arc::new(std::sync::RwLock::new(None)),
            turn_registry: Arc::new(crate::turn::queue::TurnRegistry::new()),
            model_defaults_path: None,
        };
        TurnCtx {
            app,
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            message_id: "message-1".into(),
            workspace: workspace.into(),
            model: "test-model".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
            exits: Arc::new(Mutex::new(Vec::new())),
            record: None,
            reads: Arc::new(Mutex::new(Vec::new())),
            touched: Arc::new(Mutex::new(Vec::new())),
            round_refs: Arc::new(Mutex::new(Vec::new())),
            mcp_grant: None,
            depth: 0,
        }
    }

    #[tokio::test]
    async fn the_schedule_host_fn_takes_json_in_and_returns_json_out() {
        let shared: crate::types::SharedDb = Arc::new(std::sync::Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let ctx = turn_ctx(shared.clone(), "/work/repo");
        let schedule = create_schedule_host_fn(
            &ctx,
            ScheduleDeps {
                workspace: Some(any_workspace()),
                ..Default::default()
            },
        );

        let added: Value = serde_json::from_str(
            &schedule(vec![
                "add".into(),
                serde_json::json!({"title": "t", "prompt": "p", "spec": "every:15m"}).to_string(),
            ])
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(added["nextRunAt"], serde_json::json!(t0() + 15 * MINUTE));
        // The session's own checkout is the default — a schedule made from a
        // program must not silently target the server's cwd months later.
        assert_eq!(added["workspace"], "/resolved/work/repo");
        // The calling conversation is stamped as where firings report back —
        // from the ctx, never from the wire.
        assert_eq!(added["sessionId"], "session-1");

        let listed: Value =
            serde_json::from_str(&schedule(vec!["list".into(), "null".into()]).await.unwrap())
                .unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["id"], added["id"]);

        let removed: Value = serde_json::from_str(
            &schedule(vec!["remove".into(), added["id"].to_string()])
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            removed,
            serde_json::json!({"ok": true, "removed": added["id"]})
        );
    }

    #[tokio::test]
    async fn the_schedule_host_fn_rejects_non_json_arguments_catchably() {
        let shared: crate::types::SharedDb = Arc::new(std::sync::Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let ctx = turn_ctx(shared, "/work/repo");
        let schedule = create_schedule_host_fn(&ctx, ScheduleDeps::default());
        let err = schedule(vec!["add".into(), "{not json".into()])
            .await
            .unwrap_err();
        assert_schedule_err(err, 400, "valid JSON");
    }
}
