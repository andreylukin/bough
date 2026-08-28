//! The phase's SWAP exit gate (§17): the CONSUMER of the `tools` seam is switched by a patch edit
//! while the tree is up, and nothing else moves.
//!
//! The switch is `tools.codemode.disabled`, not a row swapped for another row, and that is the
//! honest spelling of what the phase built: `bundles/bough-codemode.yml` ADDS a consumer over the
//! shipped tree rather than replacing the typed rows, so "which surface does the model see" is
//! exactly one row being enabled or not. The typed rows — `tools`, `tools.baseline`,
//! `tools.operator`, `tool.actions`, `tool.spawn_worker`, `tool.ask`, `tool.fork` — stay ACTIVE
//! throughout, because their tools are still what a program calls.

mod support;

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

    write_patch(&dir, DISABLE_CODEMODE);
    recompose(&kernel, "", &dir)
        .await
        .expect("the patch composes");
    assert_active(&kernel, &SEAM_ROWS, "with the consumer disabled");

    clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("the revert composes");
    assert_active(&kernel, &SEAM_ROWS, "after the revert");
    assert_active(&kernel, &CODEMODE_ROWS, "after the revert");
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
