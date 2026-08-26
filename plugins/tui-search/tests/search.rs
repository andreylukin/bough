//! WP-5: the FTS pane. Every test here is over a pure function or over `SearchState`, because
//! `RenderCx`, `PaneCx` and `TuiHandle` are only constructible inside `tui-shell` (D-WP5-1).

use std::collections::BTreeSet;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Class, ClassRule, LedgerHandle, Seq, Step, StepId, StepType,
    StepTypeDef, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tui_search::{
    hit_id, hit_line, hit_rows, on_click, run_query, step_of_hit, Debounce, HitRow, SearchConfig,
    SearchState,
};
use bough_plugin_tui_shell::pane::{PaneId, PaneOutcome};
use chrono::{TimeZone, Utc};

fn cfg() -> SearchConfig {
    SearchConfig {
        height: 12,
        limit: 50,
        debounce_ms: 150,
    }
}

/// A fixture step type, so the tests append real rows rather than lean on another row's
/// vocabulary. `thought/text` belongs to `agent-loop`, and this crate must not depend on it.
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Note {
    text: String,
}

const NOTE: &str = "probe/note";

fn ledger() -> LedgerHandle {
    let handle = LedgerHandle(MemoryStore::new(Context::root(KernelCore::new())));
    // The token is dropped, not spent: a registration is undone by an EFFECT, never by a `Drop`
    // (§0.2), so dropping it leaves the type registered for the test's life.
    drop(
        handle
            .0
            .register_step_type(
                StepTypeDef::of::<Note>(NOTE, "tui-search-test").class_rule(ClassRule::Thought),
            )
            .expect("`probe/note` is a fresh step type"),
    );
    handle
}

async fn put(l: &LedgerHandle, traj: &str, text: &str) -> Step {
    l.0.append(Append {
        traj: TrajId::new(traj),
        wake: WakeId::new("w1"),
        kind: StepType::new(NOTE),
        class: Class::Thought,
        body: serde_json::json!({ "text": text }),
        cites: vec![],
        at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        id: None,
    })
    .await
    .expect("append")
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
    .expect("put_agent");
}

#[tokio::test]
async fn a_query_returns_hits_across_two_trajectories() {
    let l = ledger();
    put(&l, "lane/sol", "the swap gate is the search row").await;
    put(&l, "lane/terra", "the swap gate reflows the layout").await;
    put(&l, "lane/terra", "nothing to do with it").await;

    let rows = run_query(&l, &cfg(), "swap").await.expect("query");
    assert_eq!(rows.len(), 2, "{rows:#?}");
    let trajs: BTreeSet<String> = rows.iter().map(|r| r.traj.to_string()).collect();
    assert_eq!(
        trajs,
        BTreeSet::from(["lane/sol".to_string(), "lane/terra".to_string()])
    );
}

#[tokio::test]
async fn hits_carry_the_agent_name_for_a_traj_with_an_agents_row() {
    let l = ledger();
    agent_row(&l, "sol", "lane/sol").await;
    put(&l, "lane/sol", "the swap gate").await;

    let rows = run_query(&l, &cfg(), "swap").await.expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].agent, Some(AgentName::new("sol")));
    assert!(
        hit_line(&rows[0]).starts_with("sol "),
        "{}",
        hit_line(&rows[0])
    );
}

#[tokio::test]
async fn a_rowless_trajectory_renders_without_an_agent_name() {
    let l = ledger();
    agent_row(&l, "sol", "lane/sol").await;
    // A trajectory with NO `agents` row: a subagent branch, an imported chain.
    put(&l, "scratch/import", "the swap gate").await;

    let rows = run_query(&l, &cfg(), "swap").await.expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].agent, None);
    let line = hit_line(&rows[0]);
    assert!(line.starts_with("s1 "), "{line}");
    assert!(!line.contains("sol"), "{line}");
    assert!(!line.contains("scratch/import"), "{line}");
}

fn row(step: &str, agent: Option<&str>) -> HitRow {
    HitRow {
        agent: agent.map(AgentName::new),
        traj: TrajId::new("lane/sol"),
        step: StepId::new(step),
        seq: Seq(7),
        kind: StepType::new(NOTE),
        snippet: "the swap gate".into(),
    }
}

#[test]
fn a_bad_fts_query_renders_an_inline_error_and_clears_the_list() {
    let mut state = SearchState::new(&cfg());
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

    let g = state.push_char('a', now);
    assert!(state.apply(g, Ok(vec![row("s1", Some("sol"))])));
    assert_eq!(state.rows.len(), 1);
    assert_eq!(state.error, None);

    // FTS5 rejects a dangling operator; the provider hands the message back.
    let g = state.push_char('"', now);
    assert!(state.apply(g, Err("fts5: syntax error near \"\"\"".into())));
    assert!(state.rows.is_empty(), "the list is cleared beside an error");
    assert_eq!(
        state.error.as_deref(),
        Some("fts5: syntax error near \"\"\"")
    );

    // And the error goes away as soon as a query succeeds again.
    let g = state.backspace(now);
    assert!(state.apply(g, Ok(vec![row("s1", Some("sol"))])));
    assert_eq!(state.error, None);
    assert_eq!(state.rows.len(), 1);
}

#[test]
fn the_debounce_collapses_a_burst_of_keystrokes_into_one_query() {
    let window = 150;
    let mut deb = Debounce::new(window);
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

    // Five keystrokes 10ms apart, each arming a timer that fires `window` later.
    let mut armed: Vec<(u64, chrono::DateTime<Utc>)> = Vec::new();
    for i in 0..5u32 {
        let at = t0 + chrono::Duration::milliseconds(i as i64 * 10);
        let g = deb.on_input(at);
        armed.push((g, at + chrono::Duration::milliseconds(window as i64)));
    }
    let fired: Vec<u64> = armed
        .iter()
        .filter(|(g, when)| deb.due(*g, *when))
        .map(|(g, _)| *g)
        .collect();
    assert_eq!(fired, vec![5], "exactly the last generation runs");

    // A keystroke after the window is its own query, not a suppressed one.
    let late = t0 + chrono::Duration::milliseconds(1_000);
    let g = deb.on_input(late);
    assert!(deb.due(g, late + chrono::Duration::milliseconds(window as i64)));
    // ...and a timer is never due before its window is up.
    assert!(!deb.due(g, late + chrono::Duration::milliseconds(window as i64 - 1)));
}

#[test]
fn clicking_a_hit_returns_a_focus_outcome_naming_the_step() {
    let rows = vec![row("s1", Some("sol")), row("s2", None)];
    let focus = PaneId::new("tui.focus");

    let out = on_click(
        Some(&hit_id(&StepId::new("s2"))),
        &rows,
        Some(focus.clone()),
        |_| None,
    );
    match out {
        PaneOutcome::Focus(req) => {
            assert_eq!(req.step, Some(StepId::new("s2")));
            assert_eq!(req.pane, Some(focus));
            assert_eq!(
                req.agent, None,
                "a rowless traj focuses the step, not an agent"
            );
        }
        other => panic!("expected a focus outcome, got {other:?}"),
    }

    // A hit the pane does not own, and a hit for a row that is gone, are both ignored — a click
    // never invents a step id.
    assert_eq!(
        on_click(
            Some(&bough_plugin_tui_shell::HitId::new("tool:c1")),
            &rows,
            None,
            |_| None
        ),
        PaneOutcome::Ignored
    );
    assert_eq!(
        on_click(Some(&hit_id(&StepId::new("s9"))), &rows, None, |_| None),
        PaneOutcome::Ignored
    );
    assert_eq!(on_click(None, &rows, None, |_| None), PaneOutcome::Ignored);

    // The `HitId` round-trips, which is what makes the click addressable at all.
    assert_eq!(
        step_of_hit(&hit_id(&StepId::new("s1"))),
        Some(StepId::new("s1"))
    );
}

#[tokio::test]
async fn hit_rows_is_pure_over_hits_and_agent_rows() {
    let l = ledger();
    let s = put(&l, "lane/sol", "the swap gate").await;
    let hits = vec![bough_plugin_ledger::SearchHit {
        step: s.clone(),
        snippet: "the  swap\ngate".into(),
    }];
    let agents = vec![AgentRow {
        name: AgentName::new("sol"),
        traj: TrajId::new("lane/sol"),
        routing_refs: BTreeSet::new(),
        wake_classes: BTreeSet::new(),
        model_override: None,
        tick_floor: None,
        digest_rollup: None,
    }];
    let rows = hit_rows(&hits, &agents);
    assert_eq!(rows[0].agent, Some(AgentName::new("sol")));
    assert_eq!(rows[0].step, s.id);
    // The snippet is one line: a hit row is one row.
    assert_eq!(rows[0].snippet, "the swap gate");
}
