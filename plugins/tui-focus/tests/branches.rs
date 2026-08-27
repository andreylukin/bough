//! WP-7 / §2.8, §4: the branch picker. A branch is a trajectory that left this one; a LANE is one
//! somebody lives on, a FORK is one nobody does. Switching to a branch is a pane-local trajectory
//! override, never a `FocusRequest` — a fork has no agent to focus.

use bough_plugin_ledger::{AgentName, Edge, EdgeKind, Seq, TrajId};
use bough_plugin_tui_focus::{branches_from_edges, BranchPicker, FocusState, PickerOutcome};
use crossterm::event::{KeyCode, KeyEvent};

fn edge(child: &str, parent: &str, at: u64, kind: EdgeKind) -> Edge {
    Edge {
        child: TrajId::new(child),
        parent: TrajId::new(parent),
        at_seq: Seq(at),
        kind,
        at: chrono::Utc::now(),
    }
}

fn no_lanes(_: &TrajId) -> Option<AgentName> {
    None
}

fn no_steps(_: &TrajId) -> usize {
    0
}

mod tests {
    use super::*;

    /// Oldest first, by where the branch left the parent: the list is a history, and a redraw
    /// must never reshuffle it under the cursor.
    #[test]
    fn ancestor_children_are_listed_oldest_first() {
        let edges = vec![
            edge("lane/c", "lane/sol", 90, EdgeKind::Ancestor),
            edge("lane/a", "lane/sol", 10, EdgeKind::Ancestor),
            edge("lane/b", "lane/sol", 40, EdgeKind::Ancestor),
            // A merge edge is history that flowed IN, not a branch to switch to.
            edge("lane/m", "lane/sol", 20, EdgeKind::Merge),
            // Somebody else's child.
            edge("lane/x", "lane/terra", 5, EdgeKind::Ancestor),
        ];
        let out = branches_from_edges(&edges, &TrajId::new("lane/sol"), &no_lanes, &no_steps);
        let names: Vec<String> = out.iter().map(|b| b.traj.to_string()).collect();
        assert_eq!(names, vec!["lane/a", "lane/b", "lane/c"]);
        assert_eq!(out[0].at_seq, Seq(10));
    }

    /// §4: a child with an `agents` row is a lane; one without is a fork, promotable by adding
    /// the row. The two are labelled differently because they behave differently — a fork gets no
    /// mail and no wakes.
    #[test]
    fn a_child_with_an_agents_row_is_labelled_a_lane_and_one_without_a_fork() {
        let edges = vec![
            edge("lane/luna", "lane/sol", 10, EdgeKind::Ancestor),
            edge("fork/probe", "lane/sol", 20, EdgeKind::Ancestor),
        ];
        let lane_of = |t: &TrajId| (t.as_str() == "lane/luna").then(|| AgentName::new("luna"));
        let out = branches_from_edges(&edges, &TrajId::new("lane/sol"), &lane_of, &|_| 3);
        assert_eq!(out[0].lane, Some(AgentName::new("luna")));
        assert_eq!(out[0].word(), "lane");
        assert_eq!(out[1].lane, None);
        assert_eq!(out[1].word(), "fork");
        assert_eq!(out[1].steps, 3);
    }

    /// Enter shows the branch: the pane's trajectory moves, the FOCUSED AGENT does not. A fork
    /// has no agent, so a `FocusRequest` would have nothing to name.
    #[test]
    fn selecting_a_branch_switches_the_panes_trajectory() {
        let mut state = FocusState {
            agent: Some(bough_plugin_agents::AgentId::new("sol")),
            traj: Some(TrajId::new("lane/sol")),
            ..Default::default()
        };
        let mut picker = BranchPicker::default();
        picker.open_with(branches_from_edges(
            &[
                edge("lane/luna", "lane/sol", 10, EdgeKind::Ancestor),
                edge("fork/probe", "lane/sol", 20, EdgeKind::Ancestor),
            ],
            &TrajId::new("lane/sol"),
            &no_lanes,
            &no_steps,
        ));
        assert_eq!(
            picker.on_key(KeyEvent::from(KeyCode::Down)),
            PickerOutcome::Moved
        );
        let out = picker.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(out, PickerOutcome::Show(TrajId::new("fork/probe")));
        assert!(!picker.open, "choosing closes the picker");

        state.show_branch(TrajId::new("fork/probe"), vec![]);
        assert_eq!(state.traj, Some(TrajId::new("fork/probe")));
        assert_eq!(
            state.agent,
            Some(bough_plugin_agents::AgentId::new("sol")),
            "the branch view moves the TRAJECTORY, not the focused agent"
        );
        assert!(state.on_branch());
    }

    /// Esc always gets Andrey back to the agent's own chain, whatever the picker did.
    #[test]
    fn esc_returns_to_the_agents_own_chain() {
        let mut state = FocusState {
            traj: Some(TrajId::new("lane/sol")),
            ..Default::default()
        };
        state.show_branch(TrajId::new("fork/probe"), vec![]);
        let mut picker = BranchPicker::default();
        picker.open_with(vec![]);
        assert_eq!(
            picker.on_key(KeyEvent::from(KeyCode::Esc)),
            PickerOutcome::Restore
        );
        assert!(!picker.open);

        assert!(state.restore_own_chain(vec![]));
        assert_eq!(state.traj, Some(TrajId::new("lane/sol")));
        assert!(!state.on_branch());
        // Twice is a no-op rather than a second restore to nowhere.
        assert!(!state.restore_own_chain(vec![]));
        assert_eq!(state.traj, Some(TrajId::new("lane/sol")));
    }

    /// "No branches" is an ANSWER. An agent that never split still opens the picker and is told
    /// so, rather than pressing `^b` and having nothing happen.
    #[test]
    fn an_agent_with_no_children_renders_an_empty_picker() {
        let out = branches_from_edges(
            &[edge("lane/x", "lane/terra", 5, EdgeKind::Ancestor)],
            &TrajId::new("lane/sol"),
            &no_lanes,
            &no_steps,
        );
        assert!(out.is_empty());

        let mut picker = BranchPicker::default();
        picker.open_with(out);
        assert!(picker.open);
        let theme = bough_plugin_tui_shell::Theme::of(bough_plugin_tui_shell::ThemeName::Dark);
        let text: Vec<String> = picker
            .lines(40, &theme)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(text.iter().any(|l| l.contains("no branches")), "{text:?}");
        // Enter on an empty list behaves as Esc rather than leaving Andrey stuck.
        assert_eq!(
            picker.on_key(KeyEvent::from(KeyCode::Enter)),
            PickerOutcome::Restore
        );
    }
}
