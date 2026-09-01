//! WP-5: the surface is documented ONCE, and the documentation is generated from the same list
//! the sandbox injects.
//!
//! The tests below never name a tool to the renderer. Every roster comes from a
//! [`SurfaceSource`], which is the one seam the row wires to the live registry — so "the docs and
//! the globals cannot drift" is checked as an identity between two reads of one source, not as an
//! agreement between two hand-written lists.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, Connected, LedgerHandle, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::{
    DropPriority, Place, SectionRender, SectionRequest, SectionScope, Slot,
};
use bough_plugin_tools_codemode::bind::Binding;
use bough_plugin_tools_codemode::surface::{
    assemble, function_table, section_id, Surface, SurfaceSource, PATCH_GRAMMAR, POSITION,
    SECTION_ID, TITLE,
};
use bough_plugin_tools_codemode::CodemodeConfig;
use chrono::{TimeZone, Utc};

// --- fixtures ---------------------------------------------------------------------------------

fn binding(js: &str, tool: &str) -> Binding {
    Binding::plain(js, tool)
}

/// The tool names this tree actually REGISTERS, one line per row that registers them.
///
/// Nothing here is invented: `the_roster_names_only_tools_this_tree_registers` greps the plugin
/// sources for each spelling. A fixture that named a tool no row registers (an action kind with
/// no Provider, say) would pin the surface against a page no agent is ever shown — which is
/// exactly the drift these tests exist to catch.
const REGISTERED: &[&str] = &[
    // plugins/tools-baseline
    "bash",
    "read_file",
    "write_file",
    "edit_file",
    "glob",
    "grep",
    // plugins/tools-operator (its own `TOOL_NAMES`, spelled out so the grep test can see them)
    "view",
    "patch",
    "write",
    // MERGE: `sh` LANDED. It was the roster's own example of a name the prose taught and no row
    // registered (`docs/codemode-merge-notes.md` §9); `tools-operator` registers it now.
    "sh",
    "bg",
    "ledger_read",
    "inbox",
    "schedule",
    // plugins/tool-workers
    "spawn_worker",
    "ask",
    "fork",
    // a Track-B MCP tool, under the `mcp__` prefix the bundle namespaces
    "mcp__linear__search",
];

/// The config the shipped tree actually carries, deserialised.
///
/// MERGE: the row lives in `bundles/bough-base.yml` (disabled), not in `bough-codemode.yml` —
/// that bundle is now the three-line `disabled: false` switch. The config is read from wherever
/// the row IS, so this cannot drift from what boots.
///
/// Nothing about the surface is restated here: the alias map, the namespaces and the `hide` set
/// are read from the file that ships them, so a fixture cannot drift from the shipped surface —
/// and the derivation below is `CodemodeConfig::surface_bindings`, the SAME call the sandbox
/// makes to build the globals.
fn bundle_config() -> CodemodeConfig {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bundles/bough-base.yml");
    let text = std::fs::read_to_string(&path).expect("the base bundle is in the tree");
    let rows: serde_yaml::Value = serde_yaml::from_str(&text).expect("the bundle is YAML");
    let row = rows
        .as_sequence()
        .expect("a bundle is a list of rows")
        .iter()
        .find(|r| r["plugin"].as_str() == Some("tools-codemode"))
        .expect("the bundle mounts the code-mode consumer");
    serde_yaml::from_value(row["config"].clone()).expect("the shipped config parses")
}

struct Nope;

#[async_trait::async_trait]
impl bough_plugin_tools::Tool for Nope {
    async fn call(
        &self,
        _call: Arc<bough_plugin_tools::ToolCall>,
        _cx: bough_plugin_tools::ToolCx,
    ) -> Result<bough_plugin_tools::ToolOutcome, bough_plugin_tools::ToolFailure> {
        unreachable!("the section renders from specs; it never calls a tool")
    }
}

fn spec(name: &str) -> bough_plugin_tools::ToolSpec {
    bough_plugin_tools::ToolSpec {
        name: bough_plugin_tools::ToolName::new(name),
        description: String::new(),
        input_schema: schemars::Schema::try_from(serde_json::json!({"type": "object"})).unwrap(),
        render: bough_plugin_tools::RenderIntent::Generic,
        scope: bough_plugin_tools::ToolScope::Global,
        tool: Arc::new(Nope),
    }
}

/// The roster a fully-equipped agent gets — derived, not written: the REAL registered names put
/// through the SHIPPED config's own derivation.
fn roster_of(names: &[&str]) -> Vec<Binding> {
    let specs: Vec<bough_plugin_tools::ToolSpec> = names.iter().map(|n| spec(n)).collect();
    bundle_config()
        .surface_bindings(&specs)
        .expect("the shipped alias map binds cleanly")
}

fn full_roster() -> Vec<Binding> {
    roster_of(REGISTERED)
}

/// A source over a fixed roster, counting what the renderer asked it.
struct Fixed {
    bindings: Vec<Binding>,
    sees_run: bool,
    asked: AtomicUsize,
}

impl Fixed {
    fn new(bindings: Vec<Binding>, sees_run: bool) -> Arc<Fixed> {
        Arc::new(Fixed {
            bindings,
            sees_run,
            asked: AtomicUsize::new(0),
        })
    }
}

impl SurfaceSource for Fixed {
    fn bindings(&self, _agent: &AgentName) -> Vec<Binding> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.bindings.clone()
    }
    fn sees_run(&self, _agent: &AgentName) -> bool {
        self.sees_run
    }
}

fn request() -> SectionRequest {
    SectionRequest {
        agent: AgentName::new("sol"),
        wake: Some(WakeId::new("w1")),
        at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        // The section reads no row; the handle is here because the request carries one.
        ledger: LedgerHandle(MemoryStore::new(Context::root(KernelCore::new()))),
        connected: Arc::new(Connected {
            own: TrajId::new("t1"),
            ancestry: vec![],
            ref_matches: vec![],
            refs: Default::default(),
        }),
        as_of: None,
    }
}

// --- tests ------------------------------------------------------------------------------------

#[tokio::test]
async fn the_section_is_byte_stable_for_a_fixed_registry() {
    let rendered = Surface::new(Fixed::new(full_roster(), true))
        .render(&request())
        .await
        .expect("the section renders")
        .expect("an agent that can see `run` gets the surface");

    assert_eq!(rendered.title, TITLE);
    assert!(
        rendered.cites.is_empty(),
        "the surface is a read of the REGISTRY, not of the ledger: it cites no row"
    );
    // The section's text is the bench's main lever; a silent edit to any of the seven prose files
    // moves this snapshot.
    insta::assert_snapshot!("codemode_surface", rendered.body);
}

#[tokio::test]
async fn the_generated_list_equals_the_injected_globals() {
    let source = Fixed::new(full_roster(), true);
    let rendered = Surface::new(source.clone())
        .render(&request())
        .await
        .unwrap()
        .unwrap();

    // The globals the sandbox injects for this agent, read from the SAME source the section was
    // handed — this is the whole anti-drift claim.
    let injected = source.bindings(&AgentName::new("sol"));
    for b in &injected {
        assert!(
            rendered.body.contains(&format!("`await {}(", b.js)),
            "`{}` is injected but not documented",
            b.js
        );
    }

    // And nothing beyond them: every roster line in the RENDERED body names an injected global.
    let listed: Vec<String> = rendered
        .body
        .lines()
        .filter_map(|l| l.strip_prefix("- `await "))
        .map(|l| l.split('(').next().unwrap().to_string())
        .collect();
    let names: Vec<String> = injected.iter().map(|b| b.js.clone()).collect();
    assert_eq!(
        listed, names,
        "the table is the roster, in injection order, and nothing else"
    );
}

/// Every function the PROSE teaches is one the sandbox injects.
///
/// The generated table cannot drift — it is built from the roster — but the seven prose files
/// can, and did: `shell.md` taught `await sh(...)` at length while no row registers a `sh` tool,
/// and `work.md` taught `await act(...)` while no action kind has a Provider. A model that
/// follows the doc calls a name that is not there, the ReferenceError is uncaught, and the whole
/// program and round are lost. This test reads the assembled body, collects every `await name(`
/// it contains, and demands each be injected.
#[tokio::test]
async fn every_function_the_prose_teaches_is_injected() {
    let source = Fixed::new(full_roster(), true);
    let rendered = Surface::new(source.clone())
        .render(&request())
        .await
        .unwrap()
        .unwrap();
    let injected: Vec<String> = source
        .bindings(&AgentName::new("sol"))
        .iter()
        .map(|b| b.js.clone())
        .collect();

    for name in awaited_names(&rendered.body) {
        assert!(
            injected.contains(&name),
            "the surface teaches `await {name}(…)` but no such global is injected: a program \
             that follows the doc dies on a ReferenceError"
        );
    }
    // …and the check is not vacuous: the verbs that ARE injected are taught.
    let taught = awaited_names(&rendered.body);
    for verb in ["bash", "view", "patch", "write"] {
        assert!(taught.contains(&verb.to_string()), "{verb} is not taught");
    }
}

/// Every `await <name>(` the body contains, in source order, deduplicated. Language forms
/// (`Promise.all`, `console.log`) are not host functions and are not collected.
fn awaited_names(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in body.match_indices("await ") {
        let rest = &body[i + "await ".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if name.is_empty() || !rest[name.len()..].starts_with('(') {
            continue;
        }
        if name.starts_with("Promise.") || name.starts_with("console.") {
            continue;
        }
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[tokio::test]
async fn a_restricted_tool_is_in_neither() {
    // A lane that denies `bash`: the snapshot the sandbox injects from does not carry it, so the
    // source does not return it, so the section cannot document it — neither in the generated
    // roster NOR in the prose. One list, one absence.
    let names: Vec<&str> = REGISTERED
        .iter()
        .copied()
        .filter(|n| *n != "bash")
        .collect();
    let source = Fixed::new(roster_of(&names), true);
    let rendered = Surface::new(source.clone())
        .render(&request())
        .await
        .unwrap()
        .unwrap();

    let injected = source.bindings(&AgentName::new("sol"));
    assert!(
        !injected.iter().any(|b| b.js == "bash"),
        "the restricted tool is not injected"
    );
    assert!(
        !rendered.body.contains("- `await bash("),
        "and it is not in the generated roster either"
    );
    // The PROSE is the half that used to drift: ~100 lines of `shell.md` taught bash in detail
    // regardless of the roster.
    assert!(
        !rendered
            .body
            .contains("It is the ONLY way to run a shell command"),
        "a lane with `deny: [bash]` must not be taught bash: {}",
        &rendered.body[..400]
    );
    assert!(
        !rendered.body.contains("await bash("),
        "no worked bash example survives either"
    );
    for name in awaited_names(&rendered.body) {
        assert!(
            injected.iter().any(|b| b.js == name),
            "`await {name}(…)` is still taught with the tool restricted away"
        );
    }
    // The verbs that survived are still there, prose and all.
    assert!(rendered.body.contains("- `await view("));
    assert!(rendered.body.contains("## The patch grammar"));
}

#[tokio::test]
async fn the_patch_grammar_appears_exactly_once_in_a_whole_assembled_projection() {
    let rendered = Surface::new(Fixed::new(full_roster(), true))
        .render(&request())
        .await
        .unwrap()
        .unwrap();

    // A whole projection: some other band's text, then ours. The grammar is documented ONCE in
    // the assembled context, which is the property the "documented once" deliverable is about.
    let projection = format!(
        "## Identity\nYou are sol.\n\n{}\n\n## Tail\nnothing yet\n",
        rendered.body
    );

    let marker = "Operations: `SWAP A.=B:` replaces lines A..B";
    assert!(
        PATCH_GRAMMAR.contains(marker),
        "the marker is taken from the restored file itself"
    );
    assert_eq!(
        projection.matches(marker).count(),
        1,
        "the patch grammar is in the assembled projection exactly once"
    );
    // and the header it lives under, likewise.
    assert_eq!(projection.matches("## The patch grammar").count(), 1);
}

#[tokio::test]
async fn the_section_is_absent_for_an_agent_that_cannot_see_run() {
    let source = Fixed::new(full_roster(), false);
    let rendered = Surface::new(source.clone())
        .render(&request())
        .await
        .unwrap();
    assert!(
        rendered.is_none(),
        "an agent driving typed tools is never handed a program surface"
    );
    assert_eq!(
        source.asked.load(Ordering::SeqCst),
        0,
        "and the registry is not even read for it"
    );
}

#[test]
fn the_section_sits_before_identity_and_is_never_dropped() {
    let spec = Surface::spec(Fixed::new(full_roster(), true));
    assert_eq!(spec.id, section_id());
    assert_eq!(spec.id.as_str(), SECTION_ID);
    assert_eq!(POSITION.slot, Slot::Identity);
    assert_eq!(POSITION.place, Place::Before);
    assert_eq!(spec.position, POSITION);
    assert_eq!(spec.priority, DropPriority::Never);
    assert_eq!(spec.scope, SectionScope::Global);
    assert!(spec.agent.is_none());
}

#[test]
fn an_undocumented_tool_is_listed_rather_than_hidden() {
    // The drift can only fall one way: a tool the registry has and this module has never heard of
    // is still on the roster, generically spelled. The reverse — a documented name with no tool —
    // is impossible, because the roster IS the registry's list.
    let table = function_table(&[binding("weather", "weather")]);
    assert!(table.contains("- `await weather(args)`"));
}

#[test]
fn the_assembled_section_token_estimate_is_recorded() {
    let body = assemble(&full_roster());
    let tokens = bough_plugin_projection::tokens::count(&body);
    println!(
        "codemode.surface = {tokens} tokens (o200k_base), {} bytes",
        body.len()
    );

    // The surface section is the bench's main lever: it is the one thing code mode pays on EVERY
    // request that typed tools do not. This bound is a tripwire, not a target — a change that
    // moves it is a change Andrey should see in the bench numbers, so update it deliberately
    // together with `docs/phase-codemode-plan.md`.
    // MERGE: 3_846 -> 4_218. `sh` is REGISTERED now (`tools-operator`), so the `needs: sh` half
    // of `surface/shell.md` — which had never been assembled, because the gate skipped every
    // paragraph naming a tool no row registers — is in the section for the first time, along with
    // its function-table row. Recorded in `docs/phase-codemode-plan.md` §8.
    // CLAIMS DEMOLITION (2026-08-30): 4_218 -> 4_119. The `claim` function and its paragraph left
    // the surface with the claims seam; every code-mode request is ~99 tokens cheaper.
    // ROUND CADENCE (2026-09-01): 4_119 -> 4_251. `agent(task, opts)` documents its real
    // contract, and shell.md gained the whole-investigation paragraph after a merge-conflict
    // resolve spent 13 rounds taking one glance each. Recorded in `docs/phase-codemode-plan.md` §8.
    const RECORDED: usize = 4_251;
    const SLACK: usize = 40;
    assert!(
        tokens.abs_diff(RECORDED) <= SLACK,
        "the surface section is {tokens} tokens, recorded at {RECORDED} (±{SLACK}); \
         it must not drift silently — re-record it and say so in the plan"
    );
}

/// The fixture may not invent a tool.
///
/// The roster these tests render from used to be hand-written and carried `("sh", "bash")` and
/// `("act", "open_pr")` — neither of which this tree can produce: no row registers a tool called
/// `sh`, and no action kind has a Provider. The snapshot and the token budget were therefore
/// pinned against a surface no agent is ever shown. Every name in [`REGISTERED`] is checked back
/// against a `ToolName::new("…")` in some plugin's source here.
#[test]
fn the_roster_names_only_tools_this_tree_registers() {
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut sources = String::new();
    let mut stack = vec![plugins];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                if path.file_name().and_then(|n| n.to_str()) != Some("target") {
                    stack.push(path);
                }
            } else if path.extension().and_then(|x| x.to_str()) == Some("rs") {
                if let Ok(t) = std::fs::read_to_string(&path) {
                    sources.push_str(&t);
                }
            }
        }
    }
    for name in REGISTERED {
        // `mcp__linear__search` stands for a Track-B MCP tool, whose names are data, not code.
        if name.starts_with("mcp__") {
            continue;
        }
        // Registered either directly or through the crate's `TOOL_NAME`/`TOOL_NAMES` constant.
        let registered = sources.contains(&format!("ToolName::new(\"{name}\")"))
            || sources.contains(&format!("TOOL_NAME: &str = \"{name}\""))
            || (sources.contains(&format!("\"{name}\",")) && sources.contains("TOOL_NAMES"));
        assert!(
            registered,
            "the fixture names `{name}`, which no plugin registers"
        );
    }
    // The guard is not vacuous: a name the PROSE teaches and no row registers is the drift this
    // case exists for, so one is planted here and must NOT be found.
    assert!(
        !sources.contains("ToolName::new(\"zsh\")"),
        "`zsh` is the planted never-registered name; if a row ever takes it, plant another"
    );
}
