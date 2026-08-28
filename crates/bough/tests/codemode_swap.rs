//! The phase's SWAP exit gate (§17): the CONSUMER of the `tools` seam is switched by a patch edit
//! while the tree is up, and nothing else moves.
//!
//! The switch is `tools.codemode.disabled`, not a row swapped for another row, and that is the
//! honest spelling of what the phase built: `bundles/bough-codemode.yml` ADDS a consumer over the
//! shipped tree rather than replacing the typed rows, so "which surface does the model see" is
//! exactly one row being enabled or not. The typed rows — `tools`, `tools.baseline`,
//! `tools.operator`, `tool.actions`, `tool.spawn_worker`, `tool.ask`, `tool.fork` — stay ACTIVE
//! throughout, because their tools are still what a program calls.

use crate::support;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::Kernel;
use bough_plugin_hello::trace;
use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{Tools, ToolsHandle};
use support::{boot_real, clear_patch, recompose, row, write_patch, TempDir};

/// The seam and its typed consumers. Every one of them must be ACTIVE under BOTH surfaces.
const SEAM_ROWS: [&str; 7] = [
    "tools",
    "tools.baseline",
    "tools.operator",
    "tool.actions",
    "tool.spawn_worker",
    "tool.ask",
    "tool.fork",
];

/// The three rows `bundles/bough-codemode.yml` adds.
const CODEMODE_ROWS: [&str; 3] = ["js", "js.quickjs", "tools.codemode"];

/// Turn the consumer off, leaving every other row exactly where the bundle put it.
const DISABLE_CODEMODE: &str = "\
entries:
  tools.codemode:
    disabled: true
";

fn tools(kernel: &Kernel) -> Arc<ToolsHandle> {
    kernel
        .root()
        .peek_live::<Tools>()
        .expect("`tools` is bound")
}

/// The agent whose surface is measured. It must be one that EXISTS: concealment is installed per
/// agent, at activation for the agents alive then and on `AgentCreated` for the rest, so asking
/// about a name nobody created answers with the unconcealed global set — a true answer to the
/// wrong question.
async fn the_agent(kernel: &Kernel) -> (AgentName, bough_plugin_agents::AgentDisposer) {
    use bough_plugin_agents::{Agents, CreateAgent};
    let agents = kernel
        .root()
        .peek_live::<Agents>()
        .expect("`agents` is bound");
    let name = AgentName::new("sol");
    let (_agent, disposer) = agents
        .create(CreateAgent {
            name: name.clone(),
            traj: bough_plugin_ledger::TrajId::new("lane/swap"),
            kind: bough_plugin_agents::AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the agent is created");
    // `AgentCreated` is what installs the concealment; the listener is async, so the tree has to
    // settle before the surface is the surface.
    kernel.quiesce().await;
    (name, disposer)
}

/// The tool NAMES the agent's request would carry — what the model is shown, not what exists.
fn schema_names(kernel: &Kernel, agent: &AgentName) -> BTreeSet<String> {
    tools(kernel)
        .schemas(agent)
        .into_iter()
        .map(|d| d.name.to_string())
        .collect()
}

/// Every row in the tree, at any depth, that is not ACTIVE and not merely disabled. The bullet
/// says "nothing FAILED", which is a statement about the WHOLE tree and not only the seam rows.
fn failed_rows(kernel: &Kernel) -> Vec<String> {
    fn walk(rows: &[bough_kernel::RowSnapshot], out: &mut Vec<String>) {
        for r in rows {
            if matches!(r.state, bough_kernel::FiberState::Failed) {
                out.push(format!("{} = {:?}", r.id.as_str(), r.state));
            }
            walk(&r.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&kernel.snapshot().rows, &mut out);
    out
}

fn assert_nothing_failed(kernel: &Kernel, when: &str) {
    let failed = failed_rows(kernel);
    assert!(failed.is_empty(), "rows FAILED {when}: {failed:?}");
}

fn assert_active(kernel: &Kernel, ids: &[&str], when: &str) {
    for id in ids {
        assert_eq!(
            row(kernel, id).state,
            bough_kernel::FiberState::Active,
            "row `{id}` must be ACTIVE {when} (§0.2: an enabled row that never activates is a \
             boot failure)"
        );
    }
}

/// The code-mode tree with ONE agent alive on it — the surface only exists per agent.
async fn boot_codemode(
    tag: &str,
) -> (
    Arc<Kernel>,
    TempDir,
    AgentName,
    bough_plugin_agents::AgentDisposer,
) {
    let (kernel, dir) = boot_real("codemode", &[]).await;
    assert!(kernel.quiesce().await, "{tag}: the tree must quiesce");
    let (agent, disposer) = the_agent(&kernel).await;
    (kernel, dir, agent, disposer)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_codemode_row_mounts_by_patch_and_the_seam_rows_stay_active() {
    let _guard = trace::test_lock();
    let (kernel, _dir, _agent, _d) = boot_codemode("mount").await;
    assert_active(&kernel, &CODEMODE_ROWS, "under `--profile codemode`");
    assert_active(&kernel, &SEAM_ROWS, "under `--profile codemode`");
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_is_shown_exactly_one_tool_under_code_mode() {
    let _guard = trace::test_lock();
    let (kernel, _dir, agent, _d) = boot_codemode("one-tool").await;
    let names = schema_names(&kernel, &agent);
    assert_eq!(
        names,
        BTreeSet::from([bough_plugin_tools_codemode::RUN_TOOL.to_string()]),
        "code mode shows the model ONE tool; it showed {names:?}"
    );
    kernel.shutdown().await;
}

/// THE swap: one patch line, one live recompose, the other surface on the next request.
#[tokio::test(flavor = "multi_thread")]
async fn a_patch_switches_the_consumer_and_the_next_wake_uses_the_other_surface() {
    let _guard = trace::test_lock();
    let (kernel, dir, agent, _d) = boot_codemode("swap").await;
    let before = schema_names(&kernel, &agent);
    assert_eq!(before.len(), 1, "code mode before the patch: {before:?}");

    write_patch(&dir, DISABLE_CODEMODE);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");
    let after = schema_names(&kernel, &agent);
    assert!(
        after.len() > 1 && !after.contains(bough_plugin_tools_codemode::RUN_TOOL),
        "with the consumer disabled the model sees the typed tools again: {after:?}"
    );
    for typed in ["bash", "view", "patch", "write"] {
        assert!(after.contains(typed), "`{typed}` is missing: {after:?}");
    }
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tools_seam_rows_stay_active_and_nothing_is_failed() {
    let _guard = trace::test_lock();
    let (kernel, dir, _agent, _d) = boot_codemode("seam").await;
    assert_active(&kernel, &SEAM_ROWS, "before the patch");
    assert_nothing_failed(&kernel, "before the patch");

    write_patch(&dir, DISABLE_CODEMODE);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");
    assert_active(&kernel, &SEAM_ROWS, "with the consumer disabled");
    assert_nothing_failed(&kernel, "with the consumer disabled");

    clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("the revert composes");
    assert_active(&kernel, &SEAM_ROWS, "after the revert");
    assert_active(&kernel, &CODEMODE_ROWS, "after the revert");
    assert_nothing_failed(&kernel, "after the revert");
    kernel.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_back_restores_the_typed_schemas() {
    let _guard = trace::test_lock();
    let (kernel, dir, agent, _d) = boot_codemode("restore").await;
    let code_mode = schema_names(&kernel, &agent);

    write_patch(&dir, DISABLE_CODEMODE);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");
    let typed = schema_names(&kernel, &agent);

    clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("the revert composes");
    assert_eq!(
        schema_names(&kernel, &agent),
        code_mode,
        "re-enabling the consumer must restore the code-mode schema exactly"
    );
    assert_ne!(typed, code_mode, "the two surfaces must actually differ");
    kernel.shutdown().await;
}

/// `unmounting_the_row_restores_the_typed_schemas_exactly` (V1's last bullet), said against the
/// SHIPPED headless tree: the typed schema the consumer hides is the typed schema `--profile
/// headless` shows, name for name.
#[tokio::test(flavor = "multi_thread")]
async fn unmounting_the_row_restores_the_typed_schemas_exactly() {
    let _guard = trace::test_lock();
    let (headless, _d1) = boot_real("headless", &[]).await;
    let (h_agent, h_disposer) = the_agent(&headless).await;
    let typed_headless = schema_names(&headless, &h_agent);
    h_disposer.dispose().await;
    headless.shutdown().await;

    let (kernel, dir, agent, _d) = boot_codemode("exact").await;
    write_patch(&dir, DISABLE_CODEMODE);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");
    assert_eq!(
        schema_names(&kernel, &agent),
        typed_headless,
        "with the consumer off, the code-mode profile must show exactly the headless surface"
    );
    kernel.shutdown().await;
}

/// The `program/*` vocabulary outlives the row that declared it.
///
/// A step type describes bytes already on disk. When `tools-codemode` declared its three types
/// through `ledger.declare_step_types` the declaration was an effect, so disabling the row
/// unregistered them and the next rebuild of a trajectory that had run a program died on
/// "unknown to this binary and not ignorable" — the consumer swap was ONE-WAY on a chain that had
/// used it (`docs/codemode-merge-notes.md` §10). The row now registers them for the life of the
/// binary; this is the gate that keeps it that way.
#[tokio::test(flavor = "multi_thread")]
async fn the_program_vocabulary_survives_disabling_the_row() {
    let _guard = trace::test_lock();
    let (kernel, dir, _agent, _d) = boot_codemode("vocabulary").await;
    let ledger = support::row_ctx(&kernel, "exec")
        .get::<bough_plugin_ledger::Ledger>()
        .expect("the exec row resolves the ledger");
    let known = |l: &bough_plugin_ledger::LedgerHandle| {
        l.0.step_types()
            .into_iter()
            .map(|d| d.name.as_str().to_string())
            .collect::<BTreeSet<_>>()
    };
    for kind in ["program/call", "program/result", "program/console"] {
        assert!(
            known(&ledger).contains(kind),
            "`{kind}` is not declared while the row is up"
        );
    }

    write_patch(&dir, DISABLE_CODEMODE);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");

    let after = known(&ledger);
    for kind in ["program/call", "program/result", "program/console"] {
        assert!(
            after.contains(kind),
            "`{kind}` was unregistered with the row; a trajectory that ran a program is now \
             unreadable. {after:?}"
        );
    }
    kernel.shutdown().await;
}

/// WP-6's collapse, said from the launcher: the four spellings `tool-leader` used to register are
/// gone from the catalog's own list under either consumer. `propose_claim` is NOT one of them — it
/// survives the collapse (the global tool of `claims`, shadowed in the leader's scope), so
/// asserting its absence would assert that every lane lost its claim tool.
#[test]
fn the_five_old_spellings_are_gone_from_both_consumers() {
    for gone in [
        "adopt_unsorted",
        "draft_requirement",
        "propose_structure",
        "note_timeline",
    ] {
        assert!(
            !bough_plugin_tool_leader::TOOL_NAMES.contains(&gone),
            "`{gone}` is still registered by tool-leader"
        );
    }
    assert_eq!(
        bough_plugin_tool_leader::TOOL_NAMES,
        ["propose_claim", "curate"],
        "the leader's set is two tools"
    );
}

/// V1's reachability half, said against the REAL QuickJS sandbox rather than the scripted engine
/// the crate's own tests use: every ToolSpec the typed surface would have shown the model is a
/// callable function inside a program, one of them really runs through the pipeline, and a tool
/// another row restricted is BOTH absent from the injected globals and `NotFound` at the pipeline.
#[tokio::test(flavor = "multi_thread")]
async fn every_visible_spec_is_a_function_in_the_sandbox_and_a_restricted_one_is_not() {
    use bough_plugin_ledger::WakeId;
    use bough_plugin_tools::{FailureClass, Restrict, ToolCall, ToolCallId, ToolName};

    let _guard = trace::test_lock();
    let (kernel, dir, agent, _d) = boot_codemode("reach").await;

    // The aliases the bundle declares: `claim` IS `propose_claim`, `agent` IS `spawn_worker`.
    // A tool reached under its alias is reached; a tool reached under NEITHER spelling is not.
    let bundle: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(support::repo_root().join("bundles/bough-base.yml")).unwrap(),
    )
    .unwrap();
    let rows = bundle["rows"]
        .as_sequence()
        .or_else(|| bundle.as_sequence())
        .expect("the bundle is a list of rows");
    let row = rows
        .iter()
        .find(|r| r["id"].as_str() == Some("tools.codemode"))
        .expect("the bundle carries the `tools.codemode` row");
    let aliases = row["config"]["aliases"]
        .as_mapping()
        .cloned()
        .expect("the row declares aliases");
    // An alias value is `tool`, `tool?fixed=v#args` or a `a|b|c` dispatch, so the tool it binds
    // is read out of the value rather than compared to it whole.
    let binds = |value: &str, name: &str| -> bool {
        value
            .split('|')
            .any(|part| part.split('#').next().unwrap_or(part).split('?').next() == Some(name))
    };
    let js_name = |name: &str| -> String {
        aliases
            .iter()
            .find(|(_, v)| v.as_str().map(|v| binds(v, name)).unwrap_or(false))
            .and_then(|(k, _)| k.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| name.to_string())
    };

    // The names to prove reachable: exactly the typed schemas the consumer hides.
    write_patch(&dir, DISABLE_CODEMODE);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");
    let typed = schema_names(&kernel, &agent);
    clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("the revert composes");
    assert!(typed.len() >= 4, "the typed surface is empty: {typed:?}");

    // …minus the ones the row DROPS (phase brief item 4): `bash`/`view`/`patch` cover them, and
    // `edit_file(old, new)` is a regression against the hash-anchored patch grammar. A hidden
    // tool must be absent from the sandbox AND from the surface section, while staying registered
    // and callable by a typed-tools agent — which the disabled-consumer read above just proved.
    let hidden: BTreeSet<String> = row["config"]["hide"]
        .as_sequence()
        .map(|v| {
            v.iter()
                .filter_map(|n| n.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    assert!(!hidden.is_empty(), "the row must declare what it drops");
    let typed: BTreeSet<String> = typed.difference(&hidden).cloned().collect();

    // …minus the ones the row DROPS. `hide` is the phase brief's "drop as separate functions"
    // (`bash`/`view` cover `read_file`, `glob`, `grep`; `edit_file(old, new)` is a regression
    // against the patch grammar): a hidden tool is neither injected nor documented, and it is
    // read from the same bundle row so the two lists cannot drift.
    let hidden: Vec<String> = row["config"]["hide"]
        .as_sequence()
        .map(|h| {
            h.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    assert!(!hidden.is_empty(), "the row must declare what it drops");
    let typed: Vec<String> = typed.into_iter().filter(|n| !hidden.contains(n)).collect();

    let ctx = kernel.root().clone();
    let tools = tools(&kernel);
    let run = |program: String, n: u32| {
        let ctx = ctx.clone();
        let tools = tools.clone();
        let agent = agent.clone();
        async move {
            let mut out = tools
                .execute(
                    &ctx,
                    vec![ToolCall {
                        id: ToolCallId::new(format!("call_reach_{n}")),
                        name: ToolName::new(bough_plugin_tools_codemode::RUN_TOOL),
                        args: serde_json::json!({ "program": program }),
                        agent,
                        wake: WakeId::new(format!("w{n}")),
                        step_index: n,
                    }],
                )
                .await;
            out.remove(0)
        }
    };

    // 1. every hidden typed tool is a function in the sandbox.
    let names: Vec<String> = typed.iter().map(|s| js_name(s)).collect();
    let list = serde_json::to_string(&names).unwrap();
    let probe = format!(
        "const missing = {list}.filter(n => typeof n.split('.').reduce((o,k) => o && o[k], \
         globalThis) !== 'function'); console.log('missing:' + JSON.stringify(missing));"
    );
    let r = run(probe, 1).await;
    assert!(r.ok, "the probe program failed: {:?}", r);
    assert!(
        r.content.contains("missing:[]"),
        "some visible ToolSpec is not an injected function: {}",
        r.content
    );

    // 1b. …and a DROPPED one is not a global at all, in the sandbox or in the prompt.
    let list = serde_json::to_string(&hidden).unwrap();
    let probe = format!(
        "const present = {list}.filter(n => typeof globalThis[n] !== 'undefined'); \
         console.log('present:' + JSON.stringify(present));"
    );
    let r = run(probe, 3).await;
    assert!(r.ok, "the probe program failed: {:?}", r);
    assert!(
        r.content.contains("present:[]"),
        "a dropped tool is still injected: {}",
        r.content
    );

    // 1b. …and a DROPPED one is not a global at all: the surface section does not document it,
    //     so injecting it would be the drift running the other way.
    let list = serde_json::to_string(&hidden.iter().collect::<Vec<_>>()).unwrap();
    let probe = format!(
        "const present = {list}.filter(n => typeof globalThis[n] === 'function'); \
         console.log('present:' + JSON.stringify(present));"
    );
    let r = run(probe, 3).await;
    assert!(r.ok, "the probe program failed: {:?}", r);
    assert!(
        r.content.contains("present:[]"),
        "a dropped tool was injected anyway: {}",
        r.content
    );

    // 2. one of them really runs, through the real pipeline.
    let r = run(
        "console.log('view says ' + JSON.stringify(typeof view));".to_string(),
        2,
    )
    .await;
    assert!(
        r.content.contains("view says \"function\""),
        "{}",
        r.content
    );

    // 3. a restriction another row owns removes the global AND refuses the call.
    tools
        .restrict(
            &ctx,
            &agent,
            Restrict {
                allow: None,
                deny: BTreeSet::from([ToolName::new("view")]),
            },
        )
        .await
        .expect("the restriction installs");
    let r = run(
        "console.log('view is ' + typeof view + ', bash is ' + typeof bash);".to_string(),
        3,
    )
    .await;
    assert!(
        r.content.contains("view is undefined") && r.content.contains("bash is function"),
        "the restricted tool must be the only one gone: {}",
        r.content
    );

    let mut direct = tools
        .execute(
            &ctx,
            vec![ToolCall {
                id: ToolCallId::new("call_reach_direct"),
                name: ToolName::new("view"),
                args: serde_json::json!({}),
                agent: agent.clone(),
                wake: WakeId::new("w4"),
                step_index: 4,
            }],
        )
        .await;
    let direct = direct.remove(0);
    assert!(!direct.ok, "a restricted tool must not execute");
    assert_eq!(
        direct.failure.as_ref().map(|f| f.kind),
        Some(FailureClass::NotFound),
        "the absence is enforced at the pipeline, not only in the sandbox: {direct:?}"
    );

    kernel.shutdown().await;
}
