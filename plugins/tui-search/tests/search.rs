//! WP-5 + WP-6: the search pane. Every test here is over a pure function or over `SearchState`,
//! because `RenderCx`, `PaneCx` and `TuiHandle` are only constructible inside `tui-shell`
//! (D-WP5-1).

use std::collections::BTreeSet;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Class, ClassRule, LedgerHandle, StepId, StepType, StepTypeDef,
    TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tui_search::{
    hit_id, hit_line, lines, on_click, run_query, step_of_hit, Debounce, Hit, SearchConfig,
    SearchState,
};
use bough_plugin_tui_shell::pane::{PaneId, PaneOutcome};
use chrono::{TimeZone, Utc};

fn cfg() -> SearchConfig {
    SearchConfig {
        height: 12,
        limit: 50,
        debounce_ms: 150,
        snippet_radius: 40,
        window: 400,
    }
}

/// The step type the fixture appends. `thought/text` is `agent-loop`'s, and this crate must not
/// depend on it — but the PROJECTION keys on the name, so the fixture declares the name itself.
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Thought {
    text: String,
    step_index: u32,
}

const THOUGHT: &str = "thought/text";

/// The envelope type the audit screenshotted, so a test can prove it never reaches a hit. Its
/// shape is the ledger's own `RequestHeader` — the vocabulary row already declares the name.
const HEADER: &str = "request/header";

fn header_body() -> serde_json::Value {
    serde_json::json!({
        "prompt_ver": "v1",
        "as_of": 53,
        "budget": 96000,
        "projection_digest": "",
        "sections": ["identity"],
        "tools": ["write_file"],
        "call": {},
        "composition": "tui"
    })
}

fn ledger() -> LedgerHandle {
    let handle = LedgerHandle(MemoryStore::new(Context::root(KernelCore::new())));
    // The token is dropped, not spent: a registration is undone by an EFFECT, never by a `Drop`
    // (§0.2), so dropping it leaves the type registered for the test's life.
    drop(
        handle
            .0
            .register_step_type(
                StepTypeDef::of::<Thought>(THOUGHT, "tui-search-test")
                    .class_rule(ClassRule::Thought),
            )
            .expect("`thought/text` is a fresh step type here"),
    );
    handle
}

async fn put(l: &LedgerHandle, traj: &str, kind: &str, body: serde_json::Value) -> StepId {
    l.0.append(Append {
        traj: TrajId::new(traj),
        wake: WakeId::new("w1"),
        kind: StepType::new(kind),
        class: Class::Thought,
        body,
        cites: vec![],
        at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        id: None,
    })
    .await
    .expect("the fixture appends")
    .id
}

async fn thought(l: &LedgerHandle, traj: &str, index: u32, text: &str) -> StepId {
    put(
        l,
        traj,
        THOUGHT,
        serde_json::json!({ "text": text, "step_index": index }),
    )
    .await
}

async fn agent_row(l: &LedgerHandle, name: &str, traj: &str) {
    l.0.put_agent(AgentRow {
        name: AgentName::new(name),
        traj: TrajId::new(traj),
        routing_refs: BTreeSet::new(),
        wake_classes: BTreeSet::new(),
        model_override: None,
        tick_floor: None,
        digest_rollup: None,
    })
    .await
    .expect("the fixture registers an agent");
}

#[tokio::test]
async fn a_query_returns_hits_across_two_trajectories() {
    let l = ledger();
    agent_row(&l, "sol", "lane/sol").await;
    agent_row(&l, "terra", "lane/terra").await;
    thought(&l, "lane/sol", 0, "the swap gate is green").await;
    thought(&l, "lane/terra", 0, "the swap gate held").await;
    let hits = run_query(&l, &cfg(), "swap gate").await.expect("a query");
    assert_eq!(hits.len(), 2, "{hits:#?}");
    let agents: BTreeSet<String> = hits.iter().map(|h| h.agent.as_str().to_string()).collect();
    assert_eq!(
        agents,
        ["sol".to_string(), "terra".to_string()]
            .into_iter()
            .collect()
    );
}

/// M11: FTS over ledger JSON is what put `request/header  {"as_of":53,…}` on screen. The index is
/// over RENDERED rows, so the envelope step cannot be a hit even when the query is in its body.
#[tokio::test]
async fn an_envelope_step_can_never_be_a_hit() {
    let l = ledger();
    agent_row(&l, "sol", "lane/sol").await;
    put(&l, "lane/sol", HEADER, header_body()).await;
    thought(&l, "lane/sol", 1, "the budget is fine").await;
    let hits = run_query(&l, &cfg(), "budget").await.expect("a query");
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!(hits[0].speaker, "sol");
    assert!(!hits[0].snippet.contains("as_of"), "{:?}", hits[0].snippet);
}

#[tokio::test]
async fn a_hit_names_the_step_it_came_from() {
    let l = ledger();
    agent_row(&l, "sol", "lane/sol").await;
    thought(&l, "lane/sol", 0, "nothing here").await;
    let want = thought(&l, "lane/sol", 1, "the swap gate").await;
    let hits = run_query(&l, &cfg(), "swap").await.expect("a query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].step, want);
    assert_eq!(
        step_of_hit(&hit_id(&hits[0].step)),
        Some(want),
        "the HitId round-trips"
    );
}

fn hit(step: &str, agent: &str, snippet: &str) -> Hit {
    Hit {
        step: StepId::new(step),
        agent: AgentName::new(agent),
        speaker: agent.to_string(),
        snippet: snippet.to_string(),
        at: 0..4,
    }
}

#[test]
fn a_failed_query_renders_an_inline_error_and_clears_the_list() {
    let c = cfg();
    let mut st = SearchState::new(&c);
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let g = st.push_char('a', now);
    assert!(st.apply(g, Ok(vec![hit("s1", "sol", "swap gate")])));
    assert_eq!(st.rows.len(), 1);

    let g = st.push_char('"', now);
    assert!(st.apply(g, Err("fts5: syntax error".into())));
    assert!(
        st.rows.is_empty(),
        "a stale list beside a fresh error is a lie"
    );
    let painted: Vec<String> = lines(&st, true).into_iter().map(|(t, _)| t).collect();
    assert!(painted[1].contains("fts5: syntax error"), "{painted:?}");
}

/// Minor 30: `Esc` clears the query, the hits AND the selection, in one call.
#[test]
fn esc_clears_query_hits_and_rows() {
    let c = cfg();
    let mut st = SearchState::new(&c);
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let g = st.push_char('g', now);
    st.apply(
        g,
        Ok(vec![hit("s1", "sol", "gate"), hit("s2", "sol", "gate")]),
    );
    st.step_match(true);
    assert_eq!(st.selected, 1);

    st.clear(now);
    assert_eq!(st.input, "");
    assert!(st.rows.is_empty());
    assert_eq!(st.selected, 0);
    assert_eq!(st.scroll, 0);
    assert_eq!(st.counter(), "", "an empty query counts nothing");
}

#[test]
fn the_counter_reads_n_of_n_and_n_wraps() {
    let c = cfg();
    let mut st = SearchState::new(&c);
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let g = st.push_char('g', now);
    st.apply(
        g,
        Ok(vec![
            hit("s1", "sol", "gate"),
            hit("s2", "sol", "gate"),
            hit("s3", "sol", "gate"),
        ]),
    );
    assert_eq!(st.counter(), "1 of 3");
    st.step_match(true);
    assert_eq!(st.counter(), "2 of 3");
    st.step_match(true);
    st.step_match(true);
    assert_eq!(st.counter(), "1 of 3", "`n` wraps past the end");
    st.step_match(false);
    assert_eq!(st.counter(), "3 of 3", "`N` wraps past the start");
}

/// The field wears CHROME: a label and a box, not a dim floating string.
#[test]
fn the_field_names_itself() {
    let c = cfg();
    let mut st = SearchState::new(&c);
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    st.push_char('g', now);
    let head = &lines(&st, true)[0].0;
    assert!(head.starts_with("search ["), "{head}");
}

#[test]
fn the_debounce_collapses_a_burst_of_keystrokes_into_one_query() {
    let mut d = Debounce::new(150);
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let g1 = d.on_input(t0);
    let g2 = d.on_input(t0 + chrono::Duration::milliseconds(20));
    let g3 = d.on_input(t0 + chrono::Duration::milliseconds(40));
    let late = t0 + chrono::Duration::milliseconds(400);
    assert!(!d.due(g1, late), "an older generation never fires");
    assert!(!d.due(g2, late));
    assert!(d.due(g3, late), "the newest one does");
    assert!(
        !d.due(g3, t0 + chrono::Duration::milliseconds(50)),
        "and not before the window"
    );
}

#[test]
fn clicking_a_hit_returns_a_focus_outcome_naming_the_step() {
    let rows = vec![hit("s7", "sol", "the swap gate")];
    let main = PaneId::new("tui.focus");
    let out = on_click(
        Some(&hit_id(&StepId::new("s7"))),
        &rows,
        Some(main.clone()),
        |_| None,
    );
    match out {
        PaneOutcome::Focus(req) => {
            assert_eq!(req.step, Some(StepId::new("s7")));
            assert_eq!(req.pane, Some(main));
            assert_eq!(req.agent, None, "no live handle still focuses the step");
        }
        other => panic!("a hit is a focus request, not {other:?}"),
    }

    assert!(matches!(
        on_click(
            Some(&bough_plugin_tui_shell::HitId::new("claim:accept")),
            &rows,
            None,
            |_| None
        ),
        PaneOutcome::Ignored
    ));
    assert!(matches!(
        on_click(None, &rows, None, |_| None),
        PaneOutcome::Ignored
    ));
}

#[test]
fn a_hit_line_is_the_speaker_and_the_snippet() {
    assert_eq!(
        hit_line(&hit("s1", "sol", "the swap gate")),
        "sol  the swap gate"
    );
}

#[test]
fn the_hit_list_scrolls_so_every_hit_can_be_reached() {
    let c = cfg();
    let mut st = SearchState::new(&c);
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let g = st.push_char('g', now);
    let many: Vec<Hit> = (0..40)
        .map(|i| hit(&format!("s{i}"), "sol", "gate"))
        .collect();
    st.apply(g, Ok(many));
    st.height = 12;

    for _ in 0..20 {
        st.step_match(true);
    }
    assert_eq!(st.selected, 20);
    let painted = 1 + st.rows.len();
    let top = st.top(painted, st.height);
    assert!(
        top <= st.selected + 1 && st.selected + 1 < top + st.height as usize,
        "the selected hit is inside the viewport: top {top}, selected {}",
        st.selected
    );
}
