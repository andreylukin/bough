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
use chrono::{TimeZone, Utc};

// --- fixtures ---------------------------------------------------------------------------------

fn binding(js: &str, tool: &str) -> Binding {
    Binding {
        js: js.to_string(),
        tool: tool.to_string(),
    }
}

/// The roster a fully-equipped agent gets: §3's table, in injection order.
fn full_roster() -> Vec<Binding> {
    [
        ("bash", "bash"),
        ("sh", "bash"),
        ("bg", "bg"),
        ("bg.output", "bg"),
        ("bg.kill", "bg"),
        ("view", "view"),
        ("patch", "patch"),
        ("write", "write"),
        ("ledger.search", "ledger_read"),
        ("ledger.steps", "ledger_read"),
        ("ledger.tail", "ledger_read"),
        ("inbox", "inbox"),
        ("claim", "propose_claim"),
        ("act", "open_pr"),
        ("agent", "spawn_worker"),
        ("fork", "fork"),
        ("ask", "ask"),
        ("schedule", "schedule"),
        ("mcp.linear.search", "mcp__linear__search"),
    ]
    .iter()
    .map(|(js, tool)| binding(js, tool))
    .collect()
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

    // And nothing beyond them: every roster line in the generated table names an injected global.
    let table = function_table(&injected);
    let listed: Vec<String> = table
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

#[tokio::test]
async fn a_restricted_tool_is_in_neither() {
    // A lane that denies `bash`: the snapshot the sandbox injects from does not carry it, so the
    // source does not return it, so the section cannot document it. One list, one absence.
    let roster: Vec<Binding> = full_roster()
        .into_iter()
        .filter(|b| b.tool != "bash")
        .collect();
    let source = Fixed::new(roster, true);
    let rendered = Surface::new(source.clone())
        .render(&request())
        .await
        .unwrap()
        .unwrap();

    let injected = source.bindings(&AgentName::new("sol"));
    assert!(
        !injected.iter().any(|b| b.js == "bash" || b.js == "sh"),
        "the restricted tool is not injected"
    );
    assert!(
        !rendered.body.contains("- `await bash("),
        "and it is not in the generated roster either"
    );
    assert!(
        !rendered.body.contains("- `await sh("),
        "nor its second spelling"
    );
    // The verbs that survived are still there.
    assert!(rendered.body.contains("- `await view("));
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
    const RECORDED: usize = 4_257;
    const SLACK: usize = 40;
    assert!(
        tokens.abs_diff(RECORDED) <= SLACK,
        "the surface section is {tokens} tokens, recorded at {RECORDED} (±{SLACK}); \
         it must not drift silently — re-record it and say so in the plan"
    );
}
