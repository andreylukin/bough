//! §2.8: the `/` palette, the miss notice and the plain-language lint. Every function here is
//! PURE, so these tests need no kernel, no registry and no terminal.

use bough_plugin_commands::palette::{
    echoed, filter, house_word, miss_notice, on_key, Palette, PaletteAction,
};
use bough_plugin_commands::{CommandInfo, CommandName, CommandScope};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn info(name: &str) -> CommandInfo {
    CommandInfo {
        name: CommandName::new(name),
        summary: format!("do {name}"),
        usage: format!("/{name}"),
        scope: CommandScope::Global,
    }
}

fn names(items: &[bough_plugin_commands::palette::Item]) -> Vec<String> {
    items.iter().map(|i| i.name.to_string()).collect()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The ordering rule, and the reason it exists: a growing query must never REORDER what is left,
/// or the row under the typist's cursor changes meaning between two keystrokes.
#[test]
fn filter_is_prefix_then_substring_and_stable() {
    // Registration order is deliberately not alphabetical, and `undrift` matches `drift` only as
    // a substring.
    let all = vec![
        info("undrift"),
        info("quit"),
        info("drop"),
        info("drift"),
        info("help"),
    ];

    // Empty query: everything, alphabetical.
    assert_eq!(
        names(&filter(&all, "")),
        ["drift", "drop", "help", "quit", "undrift"]
    );

    // Prefix group first, then the substring group.
    assert_eq!(names(&filter(&all, "dri")), ["drift", "undrift"]);

    // Growing the query only removes rows; the survivors keep their relative order.
    let wide = names(&filter(&all, "dr"));
    let narrow = names(&filter(&all, "dri"));
    assert_eq!(wide, ["drift", "drop", "undrift"]);
    let kept: Vec<String> = wide.into_iter().filter(|n| narrow.contains(n)).collect();
    assert_eq!(kept, narrow);

    // A leading slash is the same query: the composer holds `/dr`, not `dr`.
    assert_eq!(names(&filter(&all, "/dri")), ["drift", "undrift"]);

    // A miss is an empty list, never a fallback to everything (M12: no reserved rows).
    assert!(filter(&all, "zzz").is_empty());
}

/// Wrapping at both ends, and `Tab` completing WITHOUT dispatching.
#[test]
fn on_key_wraps_at_both_ends_and_tab_completes_without_accepting() {
    let all = vec![info("agents"), info("drift"), info("help")];
    let items = filter(&all, "");
    let mut p = Palette {
        open: true,
        query: String::new(),
        selected: 0,
    };

    // Up at the top wraps to the bottom.
    assert_eq!(
        on_key(&mut p, key(KeyCode::Up), &items),
        PaletteAction::Moved
    );
    assert_eq!(p.selected, 2);
    // Down at the bottom wraps to the top.
    assert_eq!(
        on_key(&mut p, key(KeyCode::Down), &items),
        PaletteAction::Moved
    );
    assert_eq!(p.selected, 0);

    assert_eq!(
        on_key(&mut p, key(KeyCode::Down), &items),
        PaletteAction::Moved
    );
    // Tab completes the SELECTED name and leaves the palette open.
    assert_eq!(
        on_key(&mut p, key(KeyCode::Tab), &items),
        PaletteAction::Complete(CommandName::new("drift"))
    );
    assert!(p.open, "Tab must not close the palette");
    assert_eq!(p.selected, 1, "Tab must not move the selection");

    // Enter is the one that accepts, and it closes.
    assert_eq!(
        on_key(&mut p, key(KeyCode::Enter), &items),
        PaletteAction::Accept(CommandName::new("drift"))
    );
    assert!(!p.open);

    // Anything else falls through to the composer.
    let mut p = Palette {
        open: true,
        query: String::new(),
        selected: 0,
    };
    assert_eq!(
        on_key(&mut p, key(KeyCode::Char('x')), &items),
        PaletteAction::None
    );
    assert!(p.open);
}

/// B3/M17: the typed text survives, verbatim, inside the notice that explains the miss.
#[test]
fn a_miss_names_the_text_the_suggestion_and_the_way_out() {
    let notice = miss_notice("/tmp is where…", Some("focus"));
    assert!(notice.contains("/tmp is where…"), "{notice}");
    assert!(notice.contains("did you mean"), "{notice}");
    assert!(notice.contains("/focus"), "{notice}");
    assert!(notice.contains("/help"), "{notice}");
    assert!(notice.contains("sends it as a message"), "{notice}");

    // With nothing close enough, the two parts that always apply still apply.
    let notice = miss_notice("/xyzzy", None);
    assert!(!notice.contains("did you mean"), "{notice}");
    assert!(notice.contains("/xyzzy"), "{notice}");
    assert!(notice.contains("try /help"), "{notice}");
}

/// M18: the pane shows what was typed above what it produced.
#[test]
fn output_is_echoed_under_the_line_that_produced_it() {
    let text = echoed("/drift sol", "agent: sol\nflags: none");
    assert!(text.starts_with("/drift sol\n"), "{text}");
    assert!(text.contains("flags: none"), "{text}");
}

/// The lint the four command crates run over their own summaries (M16).
#[test]
fn the_lint_rejects_house_words_and_passes_plain_language() {
    for bad in [
        "tear the tree down and leave",
        "put a lane to sleep",
        "the roster: status, trajectory, unconsumed mail",
        "reactivate and drain the wake queue",
        "distil, surface contradictions",
    ] {
        assert!(house_word(bad).is_some(), "should be rejected: {bad}");
    }
    for good in [
        "close bough",
        "show one agent's conversation",
        "list the agents, what each is doing, and how many messages are waiting",
        "show how much an agent's stated goal has moved lately",
    ] {
        assert_eq!(house_word(good), None, "should pass: {good}");
    }
}
