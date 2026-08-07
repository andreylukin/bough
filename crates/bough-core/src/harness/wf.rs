//! Host side of the workflow worker (port of the workflow half of
//! `src/harness/` + the pre-flight half of `src/workflow/run.ts`).
//!
//! Same sidecar architecture as [`super::vm`]: the script stays JavaScript
//! (`js/wf_worker.js`) and runs in a sidecar JS runtime process speaking the
//! workflow protocol as NDJSON over stdin/stdout (ARCHITECTURE §4.1). The
//! determinism traps, the stage-major structural coordinates and the two
//! combinators all stay JS — they are monkey-patching and async-context
//! propagation, and neither ports to Rust.
//!
//! [`WORKFLOW_PROGRAM_PARAMS`] is duplicated Rust-side **by design**: importing
//! the worker into the host would evaluate its `Date.now` traps in the server
//! process and break the clock for everything. The drift is pinned
//! behaviourally instead — a probe test runs a real script that prints `typeof`
//! for every name in this list.
//!
//! One sidecar process per run. The engine (`workflow/engine.rs`) owns the
//! journal, the keys, the semaphore, the pause gate and prefix replay.

use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::json;

use super::preflight::unterminated_string;
use super::protocol::{workflow_script_params, FromWorkflowWorker};
use super::vm::{worker_script_path_for, Sidecar};

const WF_WORKER_JS: &str = include_str!("js/wf_worker.js");

/// The names a script is compiled with: the three bridged verbs and `args` from
/// the frozen `WORKFLOW_SCRIPT_PARAMS`, plus the two pure combinators and
/// `console` that the worker builds worker-side.
pub fn workflow_program_params() -> Vec<&'static str> {
    let mut v = workflow_script_params();
    v.push("parallel");
    v.push("pipeline");
    v.push("console");
    v
}

/// The same list as a constant, for callers that want it without allocating.
pub const WORKFLOW_PROGRAM_PARAMS: [&str; 7] = [
    "agent", "phase", "log", "args", "parallel", "pipeline", "console",
];

/// The body the worker actually runs: `export const meta = …` demoted to a
/// plain `const`, which leaves a harmless local binding and — unlike removing
/// the statement — preserves every line number, so a syntax error's position
/// matches the script the author wrote.
pub fn workflow_body(script: &str) -> String {
    static DECL: OnceLock<regex::Regex> = OnceLock::new();
    let re =
        DECL.get_or_init(|| regex::Regex::new(r"export\s+const\s+meta\s*=").expect("static regex"));
    // `replace` (not `replace_all`) — the TS `String.replace(regex, …)` without
    // a `g` flag rewrites the first occurrence only.
    re.replace(script, "const meta =").into_owned()
}

/// Shape the model-facing message for an engine parse failure of a SCRIPT.
/// Same contract and the same two diagnostics as the program worker's
/// pre-flight, against the workflow parameter list.
pub fn workflow_syntax_error_message(why: &str, body: &str) -> String {
    // Two phrasings because two engines: JSC (Bun) says "Cannot declare a let
    // variable twice: 'x'", V8 says "Identifier 'x' has already been declared".
    // The TS original matched only V8's, so under Bun a shadowed `agent` fell
    // through to the bare engine message; matching both is what makes this
    // product-surface sentence independent of which runtime is on PATH.
    static SHADOW: OnceLock<regex::Regex> = OnceLock::new();
    let shadow = SHADOW.get_or_init(|| {
        regex::Regex::new(
            r"Cannot declare an? [a-z ]*twice: '([^']+)'|Identifier '([^']+)' has already been declared",
        )
        .expect("static regex")
    });
    if let Some(caps) = shadow.captures(why) {
        if let Some(name) = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str()) {
            if WORKFLOW_PROGRAM_PARAMS.contains(&name) {
                return format!(
                    "workflow script does not parse: {why} — `{name}` is bound in every \
                     workflow's scope, so declaring it shadows the binding. Rename your \
                     variable and call `{name}` as it is."
                );
            }
        }
    }
    match unterminated_string(body) {
        None => format!("workflow script does not parse: {why}"),
        Some(hit) => {
            let quote_word = if hit.quote == '"' { "double" } else { "single" };
            format!(
                "workflow script does not parse: {why} — line {}: a {quote_word}-quoted string \
                 is closed by a real newline.",
                hit.line
            )
        }
    }
}

/// Compile-check a script before a worker is spawned. `None` = it parses.
///
/// The parse itself happens in the sidecar (a `check` message), so the reported
/// error comes from the very engine that will compile the script — the host and
/// the worker can never disagree about what is legal.
pub async fn check_workflow_syntax(body: &str) -> Option<String> {
    match engine_check(body).await {
        Ok(None) => None,
        Ok(Some(why)) => Some(workflow_syntax_error_message(&why, body)),
        // No JS runtime / sidecar died: a script that cannot be checked cannot
        // be run either, and saying so at submit beats a worker that never
        // starts.
        Err(e) => Some(format!("workflow script cannot be checked: {e}")),
    }
}

fn script_path() -> io::Result<PathBuf> {
    worker_script_path_for("wf_worker", WF_WORKER_JS)
}

/// Parse-only round trip: spawn a sidecar, send `check`, return the engine's
/// raw error message (`None` = the script parses).
async fn engine_check(code: &str) -> io::Result<Option<String>> {
    let mut side = Sidecar::spawn_script(script_path()?).await?;
    side.post(&json!({"type": "check", "code": code}));
    let answer = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match side.lines.next_line().await {
                Ok(Some(l)) => {
                    if let Ok(FromWorkflowWorker::CheckResult { message, .. }) =
                        serde_json::from_str(&l)
                    {
                        return Ok(message);
                    }
                }
                Ok(None) => return Err(io::Error::other("sidecar exited during the syntax check")),
                Err(e) => return Err(e),
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(io::Error::other("sidecar did not answer the syntax check")));
    side.kill();
    answer
}

/// One workflow script, running in its own sidecar process.
///
/// The engine drives this from the run's message-loop task: [`WorkflowWorker::next`]
/// in a `select!` arm, and the spawned per-call tasks post their results through
/// [`WorkflowWorker::sender`], so a slow `agent` never blocks a `log` line.
pub struct WorkflowWorker {
    side: Sidecar,
}

impl WorkflowWorker {
    pub async fn spawn() -> io::Result<WorkflowWorker> {
        Ok(WorkflowWorker {
            side: Sidecar::spawn_script(script_path()?).await?,
        })
    }

    /// Start the script. `args_json` is the run's input, handed over verbatim.
    pub fn post_run(&self, code: &str, args_json: &str) -> bool {
        self.side
            .post(&json!({"type": "run", "code": code, "argsJson": args_json}))
    }

    /// Answer one bridged call. `ok: false` rejects the script's promise with
    /// `value` as the message — a failed host call is an ordinary catchable
    /// exception inside the script, never a killed worker.
    pub fn post_host_result(&self, id: u64, ok: bool, value: &str) -> bool {
        self.side
            .post(&json!({"type": "host_result", "id": id, "ok": ok, "value": value}))
    }

    /// Ask the script to stop. It rejects everything still pending with
    /// "workflow stopped" and acks with `aborted`.
    pub fn post_abort(&self) -> bool {
        self.side.post(&json!({"type": "abort"}))
    }

    /// A sender for the per-call tasks. Sends after the worker is gone land in a
    /// closed channel and are dropped.
    pub fn sender(&self) -> tokio::sync::mpsc::UnboundedSender<String> {
        self.side.sender()
    }

    /// The next protocol message. `None` = the worker's stdout closed (it died,
    /// or it was killed). Stray non-protocol stdout is dropped, not fatal.
    ///
    /// Cancel-safe: `Lines::next_line` keeps its partial line, so this may sit
    /// in a `select!` arm.
    pub async fn next(&mut self) -> Option<FromWorkflowWorker> {
        loop {
            match self.side.lines.next_line().await {
                Ok(Some(line)) => {
                    if let Ok(msg) = serde_json::from_str::<FromWorkflowWorker>(&line) {
                        return Some(msg);
                    }
                }
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Whatever the sidecar wrote to stderr — the `workflow worker error:` path.
    pub fn stderr_text(&self) -> String {
        self.side.stderr_text()
    }

    /// Kill the sidecar. `terminate()` in TS terms: the script stops, and the
    /// run's abort signal is what interrupts the subagent TURNS.
    pub fn kill(&mut self) {
        self.side.kill();
    }

    /// The backstop after the abort handshake: kill the whole process group.
    pub fn kill_group(&mut self) {
        self.side.kill_group();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    type HostAnswer = Result<String, String>;

    /// Drive one script to completion with a canned host, and return
    /// `(result_json, agent calls seen, log/phase lines)`. A REAL sidecar: the
    /// things that can go wrong here are the traps and the coordinates, and a
    /// fake bridge would prove neither.
    async fn run_script(
        code: &str,
        args_json: &str,
        mut answer: impl FnMut(&str, &[serde_json::Value], Option<&str>) -> HostAnswer,
    ) -> (HostAnswer, Vec<(String, Option<String>)>, Vec<String>) {
        let mut w = WorkflowWorker::spawn().await.expect("sidecar");
        assert!(w.post_run(code, args_json));
        let mut calls: Vec<(String, Option<String>)> = Vec::new();
        let mut logs: Vec<String> = Vec::new();
        let out = loop {
            match tokio::time::timeout(Duration::from_secs(20), w.next()).await {
                Err(_) => break Err("the script never finished".to_string()),
                Ok(None) => break Err(format!("the sidecar died: {}", w.stderr_text())),
                Ok(Some(FromWorkflowWorker::Done { result_json })) => break Ok(result_json),
                Ok(Some(FromWorkflowWorker::Error { message, .. })) => break Err(message),
                Ok(Some(FromWorkflowWorker::Aborted)) => continue,
                Ok(Some(FromWorkflowWorker::CheckResult { .. })) => continue,
                Ok(Some(FromWorkflowWorker::Host {
                    id,
                    fn_name,
                    args,
                    pos,
                })) => {
                    let first = args
                        .first()
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if fn_name == "log" || fn_name == "phase" {
                        logs.push(first);
                    } else {
                        calls.push((first, pos.clone()));
                    }
                    match answer(&fn_name, &args, pos.as_deref()) {
                        Ok(v) => w.post_host_result(id, true, &v),
                        Err(e) => w.post_host_result(id, false, &e),
                    };
                }
            }
        };
        w.kill();
        (out, calls, logs)
    }

    fn echo(_fn: &str, args: &[serde_json::Value], _pos: Option<&str>) -> HostAnswer {
        Ok(args
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string())
    }

    fn positions(calls: &[(String, Option<String>)]) -> HashMap<String, String> {
        calls
            .iter()
            .map(|(p, pos)| (p.clone(), pos.clone().unwrap_or_default()))
            .collect()
    }

    // ---- the param list ----------------------------------------------------

    /// The drift pin (ARCHITECTURE §4.2). Asked from INSIDE the script, because
    /// `typeof x` on an undeclared identifier is "undefined" rather than a
    /// ReferenceError — a name the worker forgot to bind shows up as a hole
    /// instead of blowing the script up. This replaces the TS shared-import
    /// invariant between `run.ts` and `wf_worker.ts`.
    #[tokio::test]
    async fn the_worker_binds_exactly_workflow_program_params() {
        let probe = WORKFLOW_PROGRAM_PARAMS
            .iter()
            .map(|n| format!("[{n:?}, typeof {n}]"))
            .collect::<Vec<_>>()
            .join(",");
        let (out, _, _) = run_script(&format!("return [{probe}]"), "null", echo).await;
        let seen: Vec<(String, String)> =
            serde_json::from_str(&out.expect("the script ran")).unwrap();
        let seen: HashMap<String, String> = seen.into_iter().collect();

        assert_eq!(seen.len(), WORKFLOW_PROGRAM_PARAMS.len());
        for name in WORKFLOW_PROGRAM_PARAMS {
            let t = seen
                .get(name)
                .unwrap_or_else(|| panic!("{name} is in WORKFLOW_PROGRAM_PARAMS but not in scope"));
            // `args` is the one that may legitimately be anything, including
            // null — the others must be callable.
            if name == "args" {
                continue;
            }
            assert!(
                t == "function" || t == "object",
                "{name} bound as {t}, which a script cannot call"
            );
        }
        // And the two spellings of the list agree with each other.
        assert_eq!(workflow_program_params(), WORKFLOW_PROGRAM_PARAMS.to_vec());
    }

    // ---- determinism traps -------------------------------------------------

    /// Journal replay is only sound if the script is deterministic. Each trap
    /// says what to do instead — the message is the product surface.
    #[tokio::test]
    async fn every_nondeterministic_source_throws_with_the_fix() {
        let cases: &[(&str, &str, &str)] = &[
            ("Date.now()", "return Date.now()", "args"),
            ("new Date()", "return new Date().toISOString()", "args"),
            ("Math.random()", "return Math.random()", "index"),
            ("performance.now()", "return performance.now()", "args"),
            ("crypto.randomUUID()", "return crypto.randomUUID()", "index"),
            (
                "crypto.getRandomValues()",
                "return crypto.getRandomValues(new Uint8Array(4))[0]",
                "index",
            ),
        ];
        for (what, code, advice) in cases {
            let (out, _, _) = run_script(code, "null", echo).await;
            let err = out.expect_err(what);
            assert!(
                err.contains("not available inside a workflow"),
                "{what}: {err}"
            );
            assert!(err.contains("deterministic"), "{what}: {err}");
            assert!(err.contains(advice), "{what}: {err}");
        }
    }

    /// Only the ARGLESS construction is denied — a script may still format a
    /// timestamp it was handed through `args`.
    #[tokio::test]
    async fn a_date_built_from_args_still_works() {
        let (out, _, _) = run_script(
            "return new Date(args.at).toISOString()",
            r#"{"at": 0}"#,
            echo,
        )
        .await;
        assert_eq!(out.unwrap(), "\"1970-01-01T00:00:00.000Z\"");
    }

    /// A script ends by returning, and `process.exit()` would end the sidecar
    /// with nothing to report.
    #[tokio::test]
    async fn process_exit_is_a_catchable_error_not_a_dead_worker() {
        let (out, _, _) = run_script(
            "try { process.exit(1) } catch (e) { return e.message } return 'not trapped'",
            "null",
            echo,
        )
        .await;
        let msg = out.expect("the script survived");
        assert!(msg.contains("not available in a workflow"), "{msg}");
        assert!(msg.contains("ends by returning"), "{msg}");
    }

    // ---- structural coordinates -------------------------------------------

    /// The row-3.8 gate: coordinates come from the script's SHAPE, and
    /// `pipeline` frames are STAGE-MAJOR. Under arrival-order numbering the
    /// stage-2 cells transpose whenever stage-1 latency is skewed, and an
    /// unchanged relaunch re-bills every call past stage 1.
    #[tokio::test]
    async fn pipeline_coordinates_are_stage_major_and_latency_independent() {
        let code = r#"
          return await pipeline(
            args.items,
            (item) => agent(`s1 ${item}`),
            (prev) => agent(`s2 ${prev}`),
          )
        "#;
        let (out, calls, _) = run_script(code, r#"{"items":["A","B"]}"#, echo).await;
        assert!(out.is_ok(), "{out:?}");

        let by_prompt = positions(&calls);
        // base 0 (the pipeline's own slot), then STAGE, then item, then the
        // call's own slot inside that cell's frame.
        assert_eq!(by_prompt["s1 A"], "0.0.0.0");
        assert_eq!(by_prompt["s1 B"], "0.0.1.0");
        assert_eq!(by_prompt["s2 s1 A"], "0.1.0.0");
        assert_eq!(by_prompt["s2 s1 B"], "0.1.1.0");
        // Every stage-1 cell sorts before every stage-2 cell — structural order
        // implies causal order, which is what the replay frontier needs.
        assert_eq!(
            crate::workflow::pos::compare_pos(&by_prompt["s1 B"], &by_prompt["s2 s1 A"]),
            std::cmp::Ordering::Less
        );
    }

    /// `parallel` numbers by SLOT, not by completion, and a bare call draws from
    /// the enclosing frame's counter — a sequential script's coordinates are
    /// exactly the old monotonic numbering.
    #[tokio::test]
    async fn parallel_slots_and_bare_calls_number_structurally() {
        let code = r#"
          await agent('first')
          await parallel([() => agent('p0'), () => agent('p1')])
          await agent('last')
          return 'ok'
        "#;
        let (out, calls, _) = run_script(code, "null", echo).await;
        assert!(out.is_ok(), "{out:?}");
        let by_prompt = positions(&calls);
        assert_eq!(by_prompt["first"], "0");
        assert_eq!(by_prompt["p0"], "1.0.0");
        assert_eq!(by_prompt["p1"], "1.1.0");
        assert_eq!(by_prompt["last"], "2");
    }

    // ---- combinator semantics ---------------------------------------------

    #[tokio::test]
    async fn parallel_maps_a_thrower_to_null_and_never_rejects() {
        let code = r#"
          let rejected = false
          const out = await parallel([
            () => agent('first'),
            () => agent('boom'),
            () => { throw new Error('a thunk that throws synchronously') },
            () => 'a plain value',
          ]).catch(() => { rejected = true; return ['REJECTED'] })
          return { out, rejected }
        "#;
        let (out, _, _) = run_script(code, "null", |_fn, args, _pos| {
            let prompt = args.first().and_then(|v| v.as_str()).unwrap_or_default();
            if prompt == "boom" {
                Err("the subagent failed".to_string())
            } else {
                Ok(format!("report: {prompt}"))
            }
        })
        .await;
        let value: serde_json::Value = serde_json::from_str(&out.expect("done")).unwrap();
        assert_eq!(value["rejected"], false, "parallel() must never reject");
        assert_eq!(
            value["out"],
            json!(["report: first", null, null, "a plain value"])
        );
    }

    #[tokio::test]
    async fn a_throwing_pipeline_stage_drops_that_item_and_skips_its_rest() {
        let code = r#"
          return await pipeline(
            args.items,
            (item) => agent(`s1 ${item}`),
            (prev) => agent(`s2 ${prev}`),
          )
        "#;
        let (out, calls, _) = run_script(code, r#"{"items":["A","B"]}"#, |_fn, args, _pos| {
            let prompt = args.first().and_then(|v| v.as_str()).unwrap_or_default();
            if prompt == "s1 B" {
                Err("the subagent failed".to_string())
            } else {
                Ok(prompt.to_string())
            }
        })
        .await;
        let value: serde_json::Value = serde_json::from_str(&out.expect("done")).unwrap();
        assert_eq!(value, json!(["s2 s1 A", null]), "results keep input order");
        assert!(
            !calls.iter().any(|(p, _)| p.contains("s2 s1 B")),
            "B's remaining stages must be skipped: {calls:?}"
        );
    }

    /// `phase`/`log` never block and never reject the script.
    #[tokio::test]
    async fn phase_and_log_are_fire_and_forget_even_when_the_host_refuses() {
        let code = r#"
          phase('Review')
          log({ a: 1 })
          console.warn('from console')
          return 'finished anyway'
        "#;
        let (out, _, logs) = run_script(code, "null", |fn_name, _args, _pos| {
            if fn_name == "log" || fn_name == "phase" {
                Err("the bus is wedged".to_string())
            } else {
                Ok(String::new())
            }
        })
        .await;
        assert_eq!(out.unwrap(), "\"finished anyway\"");
        assert_eq!(logs, vec!["Review", r#"{"a":1}"#, "from console"]);
    }

    /// A run with unparseable input still runs, with `args` null — the script's
    /// own guards are a better error than a dead worker.
    #[tokio::test]
    async fn unparseable_args_run_with_null_rather_than_dying() {
        let (out, _, _) = run_script("return args === null", "{not json", echo).await;
        assert_eq!(out.unwrap(), "true");
    }

    /// `abort` rejects everything pending with the message the engine's stop
    /// path uses, and acks — the host's wind-down is one handshake.
    #[tokio::test]
    async fn abort_rejects_pending_calls_and_acks() {
        let mut w = WorkflowWorker::spawn().await.expect("sidecar");
        assert!(w.post_run(
            "try { await agent('parked') } catch (e) { return e.message }",
            "null"
        ));
        // Wait for the call to arrive, then abort instead of answering it.
        let arrived = tokio::time::timeout(Duration::from_secs(20), w.next())
            .await
            .unwrap();
        assert!(matches!(arrived, Some(FromWorkflowWorker::Host { .. })));
        assert!(w.post_abort());

        let mut acked = false;
        let mut result = None;
        while result.is_none() {
            match tokio::time::timeout(Duration::from_secs(20), w.next())
                .await
                .unwrap()
            {
                Some(FromWorkflowWorker::Aborted) => acked = true,
                Some(FromWorkflowWorker::Done { result_json }) => result = Some(result_json),
                Some(_) => continue,
                None => panic!("the sidecar died before answering"),
            }
        }
        assert!(acked, "the worker must ack the abort");
        assert_eq!(result.unwrap(), "\"workflow stopped\"");
        w.kill();
    }

    // ---- the pre-flight ----------------------------------------------------

    #[tokio::test]
    async fn a_clean_script_parses_and_a_shadowed_binding_explains_itself() {
        assert_eq!(
            check_workflow_syntax("const x = await agent('hi')\nreturn x").await,
            None
        );

        let msg = check_workflow_syntax("let agent = 1;\nreturn 0")
            .await
            .expect("shadowing a bound name is caught");
        assert!(msg.contains("workflow script does not parse"), "{msg}");
        assert!(msg.contains("bound in every workflow's scope"), "{msg}");
        assert!(msg.contains("`agent`"), "{msg}");
    }

    #[tokio::test]
    async fn a_newline_closed_string_names_its_line() {
        let msg = check_workflow_syntax("const p = \"one\ntwo\";")
            .await
            .expect("a newline-closed string is caught");
        assert!(msg.contains("line 1"), "{msg}");
        assert!(
            msg.contains("double-quoted string is closed by a real newline"),
            "{msg}"
        );
    }

    /// The body is what the worker compiles: `export` is illegal inside the
    /// function body, and demoting keeps every line number.
    #[tokio::test]
    async fn workflow_body_demotes_the_export_and_keeps_line_numbers() {
        let script = "export const meta = {\n  name: 'n',\n  description: 'd',\n}\nreturn 1\n";
        let body = workflow_body(script);
        assert!(body.starts_with("const meta = {"), "{body}");
        assert_eq!(body.split('\n').count(), script.split('\n').count());
        assert!(
            check_workflow_syntax(script).await.is_some(),
            "the raw script is not a body"
        );
        assert_eq!(check_workflow_syntax(&body).await, None);
    }
}
