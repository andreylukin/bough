//! The review findings, one named case each.
//!
//! Every case here failed on the code before its fix: an invariant whose central clause could not
//! fire, a config that booted green and degraded every round, a Consumer that knew one Provider's
//! tool names as literals, and a mirror that answered `ask` differently from the real seam.

mod support;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bough_kernel::Plugin;
use bough_plugin_tools::{
    ApprovalHandle, ApprovalOutcome, Approver, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome,
    ToolsPreExecute,
};
use bough_plugin_tools_codemode::{bind, invariant, CodemodeConfig, CodemodePlugin, ConcealMode};
use support::{agent, config, harness, spec, Echo};

/// `invariant::seen()` is a process global; these cases read it, so they take it in turn.
static SEEN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn echo() -> Arc<dyn Tool> {
    Arc::new(Echo { concludes: false })
}

fn names(bindings: &[bind::Binding]) -> Vec<String> {
    bindings.iter().map(|b| b.js.clone()).collect()
}

// ---- F1: the invariant's central clause must be able to fire ---------------------------------

/// The two halves of the invariant must come from DIFFERENT places.
///
/// `run` used to record `console: console.clone(), result_content: console.clone()` — the same
/// String on both sides — so `console != result_content` could never fire and the crate's
/// headline claim was proven by nothing. Here the console is re-read from the durable
/// `program/console` rows and the result content is the bytes the model actually received, which
/// on a thrown program are the console PLUS the terminal message: the two are observably
/// different, and the invariant still passes because the difference is the recorded error.
#[tokio::test]
async fn the_observation_reads_the_console_from_the_ledger_and_the_result_from_the_model_bytes() {
    let _seen = SEEN.lock().await;
    invariant::clear();
    let h = harness(vec![spec("echo", echo())], config()).await;
    let failed = h
        .program("log printed\ncall echo [{\"n\":1}]\nthrow boom")
        .await
        .expect_err("the program throws");

    let obs = invariant::seen();
    let obs = obs.last().expect("the program was observed");
    assert_eq!(obs.calls, vec![0], "the calls are read back off the ledger");
    assert_eq!(obs.results, vec![0]);

    // The ledgered console really is the ledger's, not the buffer the result was built from.
    let ledgered: String = h
        .steps("program/console")
        .await
        .iter()
        .filter_map(|s| s.body["text"].as_str().map(str::to_string))
        .collect();
    assert_eq!(obs.console, ledgered);

    assert_ne!(
        obs.console, obs.result_content,
        "on a failing program the model sees the console PLUS the terminal message; recording \
         the same String twice is what made this clause unfalsifiable"
    );
    assert_eq!(obs.result_content, failed.message);
    assert_eq!(obs.error.as_deref(), Some("uncaught: boom"));
    invariant::evaluate(std::slice::from_ref(obs)).expect("the ledger reconstructs the result");
    invariant::clear();
}

/// And the clean path: no terminal message, so the ledgered console IS the result, byte for byte.
#[tokio::test]
async fn a_clean_program_is_reconstructed_from_the_ledgered_console_alone() {
    let _seen = SEEN.lock().await;
    invariant::clear();
    let h = harness(vec![spec("echo", echo())], config()).await;
    let out = h.program("log a\ncall echo []\nlog b").await.unwrap();
    let obs = invariant::seen();
    let obs = obs.last().expect("the program was observed");
    assert!(obs.error.is_none());
    assert_eq!(obs.result_content, out.content);
    assert_eq!(obs.console, out.content);
    invariant::evaluate(std::slice::from_ref(obs)).expect("clean");
    invariant::clear();
}

// ---- F4/F5: misconfiguration fails at LOAD ---------------------------------------------------

fn cfg() -> CodemodeConfig {
    config()
}

#[test]
fn an_alias_that_is_not_a_js_identifier_is_rejected_at_load() {
    let mut c = cfg();
    c.aliases
        .insert("ledger-search".to_string(), "ledger_read".to_string());
    let e = <CodemodePlugin as Plugin>::validate(&c)
        .expect_err("a name the sandbox cannot inject is a boot failure, not a bad round");
    assert!(format!("{e}").contains("ledger-search"), "{e}");

    // …and the legal spelling still loads.
    let mut ok = cfg();
    ok.aliases.insert(
        "ledger.search".to_string(),
        "ledger_read?op=search#q".to_string(),
    );
    <CodemodePlugin as Plugin>::validate(&ok).expect("a legal alias loads");
}

#[test]
fn a_namespace_that_can_never_bind_anything_is_rejected_at_load() {
    let mut empty = cfg();
    empty.namespaces.insert("act".to_string(), String::new());
    <CodemodePlugin as Plugin>::validate(&empty)
        .expect_err("an empty prefix claims nothing: an enabled row that never activates");

    let mut clash = cfg();
    clash
        .namespaces
        .insert("mcp".to_string(), "mcp__".to_string());
    clash.aliases.insert("mcp".to_string(), "inbox".to_string());
    <CodemodePlugin as Plugin>::validate(&clash)
        .expect_err("a namespace object and a function cannot own the same global");
}

#[test]
fn conceal_seam_is_rejected_at_load_because_the_seam_call_does_not_exist() {
    let mut c = cfg();
    c.conceal = ConcealMode::Seam;
    let e = <CodemodePlugin as Plugin>::validate(&c).expect_err(
        "`seam` used to boot green and then leave every later-created agent unconcealed",
    );
    assert!(format!("{e}").contains("seam"), "{e}");
}

#[test]
fn an_impossible_tag_window_is_rejected_at_load() {
    let mut c = cfg();
    c.tags_min = 6;
    c.tags_max = 5;
    <CodemodePlugin as Plugin>::validate(&c).expect_err("no call could ever satisfy it");
    let mut zero = cfg();
    zero.max_parallel_calls = 0;
    <CodemodePlugin as Plugin>::validate(&zero).expect_err("a batch of nothing");
}

// ---- F9: the dropped verbs are dropped from BOTH lists ---------------------------------------

/// The phase brief drops `glob`, `grep` and `read_file` as separate functions, and `edit_file`
/// as a regression against the patch grammar. They used to be injected AND documented, because
/// `tools-baseline` stays mounted under `profiles/codemode.yml`; `hide` drops them in the one
/// derivation both lists come from.
#[tokio::test]
async fn hidden_tools_are_neither_injected_nor_documented() {
    let mut c = config();
    c.hide = ["read_file", "glob", "grep", "edit_file", "write_file"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let specs = vec![
        spec("bash", echo()),
        spec("read_file", echo()),
        spec("glob", echo()),
        spec("grep", echo()),
        spec("edit_file", echo()),
        spec("write_file", echo()),
        spec("view", echo()),
    ];
    let injected = c.surface_bindings(&specs).expect("the surface builds");
    assert_eq!(
        names(&injected),
        vec!["bash".to_string(), "view".to_string()]
    );

    // The tool is hidden, never revoked: it is still registered, and an agent this row does not
    // conceal still sees it.
    let mut open = c.clone();
    open.conceal = ConcealMode::None;
    let h = harness(specs, open).await;
    assert!(
        h.tools
            .resolve(&agent(), &bough_plugin_tools::ToolName::new("read_file"))
            .is_ok(),
        "concealment and hiding are VISIBILITY, never authority"
    );
}

// ---- F8: no Provider's tool names as literals ------------------------------------------------

/// A tool that records the arguments it was handed and answers with text plus a value.
struct Recorder(parking_lot::Mutex<Vec<serde_json::Value>>);

#[async_trait::async_trait]
impl Tool for Recorder {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        self.0.lock().push(call.args.clone());
        Ok(ToolOutcome {
            content: "the combined output".to_string(),
            value: Some(serde_json::json!({ "exit_code": 0 })),
            cites: vec![],
            concludes_wake: false,
        })
    }
}

/// Swap the shell Provider for one that registers `shell` instead of `bash`, and every shell rule
/// must still apply: the tag requirement, the tag stripping, and the documented string return.
/// All three used to be `name == "bash"` literals in a crate that does not even depend on
/// `tools-baseline`.
#[tokio::test]
async fn the_shell_rules_follow_the_config_not_the_name_bash() {
    let rec = Arc::new(Recorder(parking_lot::Mutex::new(Vec::new())));
    let mut c = config();
    c.shell_tools = BTreeSet::from(["shell".to_string()]);
    c.shell_content_result = BTreeSet::from(["shell".to_string()]);
    c.tags_required = true;
    let h = harness(vec![spec("shell", rec.clone())], c).await;

    // Untagged: refused by the tag rule, and it never ran.
    let out = h.program("call shell [\"ls\"]").await.unwrap();
    assert!(out.content.contains("Denied"), "{}", out.content);
    assert!(rec.0.lock().is_empty(), "a refused leg is not a call");

    // Tagged: the tags are taken off before binding, and the STRING content is what comes back —
    // not the `{exit_code}` value, which is what the generic "a value wins" rule would return.
    let out = h
        .program("call shell [\"ls\",\"repo:layout:probe\"]")
        .await
        .unwrap();
    assert!(
        out.content.contains("ok \"the combined output\""),
        "the surface promises a shell call returns its output as a string: {}",
        out.content
    );
    let seen = rec.0.lock().clone();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].get("tags").is_none(),
        "the tag argument is code mode's, not the tool's: {:?}",
        seen[0]
    );
}

// ---- F6: the mirror answers `ask` the way the real seam does ---------------------------------

struct Yes;

#[async_trait::async_trait]
impl Approver for Yes {
    async fn ask(&self, _call: &ToolCall, _reason: &str) -> ApprovalOutcome {
        ApprovalOutcome::Allow
    }
}

/// An `ask` decision inside a program used to degrade to deny even with an approver mounted: the
/// mirror is a fresh `ToolsInner` whose `approval` is `None`, and `exec` reads the MIRROR's.
#[tokio::test]
async fn an_ask_inside_a_program_reaches_the_mounted_approver() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    let _mounted = h
        .tools
        .mount_approval(&h.ctx, ApprovalHandle(Arc::new(Yes)))
        .await
        .expect("the approver mounts");
    let _gate = h
        .ctx
        .on_waterfall::<ToolsPreExecute, _, _>(|mut v, _next| async move {
            v.ask("a program asked");
            v
        })
        .await
        .expect("the listener registers");

    let out = h.program("call echo []").await.unwrap();
    assert!(
        out.content.starts_with("ok "),
        "the approver said allow, so the inner call must run: {}",
        out.content
    );
    assert!(
        !out.content.contains("no approver is mounted"),
        "the mirror must not answer `ask` differently from the real handle: {}",
        out.content
    );
}

// ---- F7: the seam's limits are config, and parallelism is enforced ---------------------------

/// A tool that reports the highest number of its own calls that were ever in flight at once.
struct Concurrent {
    live: AtomicUsize,
    peak: AtomicUsize,
}

#[async_trait::async_trait]
impl Tool for Concurrent {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutcome::default())
    }
}

/// `tools.max_parallel` never applied under code mode: each host call issues its own single-call
/// `execute_under`, so the program's `RwLock` read guard admitted every concurrency-safe call at
/// once. `max_parallel_calls` is the knob, and it is enforced where the calls actually overlap.
#[tokio::test(flavor = "multi_thread")]
async fn concurrency_safe_inner_calls_are_capped_by_max_parallel_calls() {
    let tool = Arc::new(Concurrent {
        live: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    });
    let mut c = config();
    // No concealment: the mirror here is the harness registry itself, and a restricted handle
    // would answer `NotFound` for the very tool under test.
    c.conceal = ConcealMode::None;
    c.max_parallel_calls = 2;
    let specs = vec![spec("wide", tool.clone())];
    let h = harness(specs.clone(), c.clone()).await;

    let pcx = bind::ProgramCx::new(
        h.ctx.clone(),
        h.ledger.clone(),
        support::traj(),
        bough_plugin_ledger::WakeId::new("w1"),
        agent(),
        1,
        bough_plugin_tools::ToolCallId::new("call_1"),
        h.tools.clone(),
        tokio_util::sync::CancellationToken::new(),
        64,
        c.shell_rules(),
        c.max_parallel_calls,
    );
    let by_name: std::collections::BTreeMap<String, &bough_plugin_tools::ToolSpec> =
        specs.iter().map(|s| (s.name.to_string(), s)).collect();
    let binding = bind::Binding::plain("wide", "wide");
    let f = bind::host_fn(&binding, &by_name, pcx)
        .expect("the global binds")
        .body;

    let legs = (0..6).map(|_| {
        let f = f.clone();
        tokio::spawn(async move { f.call(vec![]).await })
    });
    for leg in legs.collect::<Vec<_>>() {
        assert!(
            leg.await.expect("the leg joins").is_ok(),
            "every leg succeeds"
        );
    }
    let peak = tool.peak.load(Ordering::SeqCst);
    assert!(
        peak <= 2,
        "six concurrency-safe calls overlapped {peak} at a time under a limit of 2"
    );
    assert!(peak > 1, "the limiter must not serialise everything either");
}
