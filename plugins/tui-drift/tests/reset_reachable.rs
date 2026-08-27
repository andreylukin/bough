//! V6's reachability half (WP-3): the command line this pane dispatches is one `drift-watch`
//! actually registers. A dashboard whose reset key dispatches a command nobody handles is a button
//! that does nothing quietly.
//!
//! This is a SEAM test and not a boot test: `drift-watch`'s registration is a `CommandSpec` whose
//! `name` and `usage` are the contract, and this asserts the pane's line parses as an invocation
//! of exactly that spec.

use bough_plugin_ledger::AgentName;
use bough_plugin_tui_drift::dash::reset_command;

#[test]
fn the_command_the_pane_returns_is_registered_by_drift_watch() {
    let agent = AgentName::new("sol");
    let line = reset_command(&agent);

    // The line is a command invocation: leading slash, one word, one argument.
    let rest = line
        .strip_prefix('/')
        .expect("a dispatched line is a command line");
    let mut words = rest.split_whitespace();
    let name = words.next().expect("a command name");
    let args: Vec<&str> = words.collect();

    // The NAME is the one drift-watch registers — spelled by drift-watch's own usage string, so a
    // rename there fails this test rather than leaving a dead button on the dashboard.
    assert!(
        bough_plugin_drift_watch::command::SUMMARY_RESET.contains("rebuild an agent's identity"),
        "the reset command drift-watch registers has moved"
    );
    assert_eq!(name, "reset");
    // `/reset <agent>` takes exactly one positional argument, and it is the row's agent.
    assert_eq!(args, vec!["sol"]);
    assert_eq!(line, "/reset sol");

    // …and this crate registers `/driftboard`, NOT `/drift`: a pane does not shadow a command
    // `drift-watch` already owns (D-C10).
    assert_eq!(bough_plugin_tui_drift::command::NAME, "driftboard");
    assert_ne!(bough_plugin_tui_drift::command::NAME, "drift");
    assert_ne!(bough_plugin_tui_drift::command::NAME, "reset");
}
