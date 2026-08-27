//! §0.2: **misconfiguration fails loud at load when self-contained.** Every row this phase adds
//! implements `Plugin::validate`, so a nonsense value is a boot failure rather than a silent
//! clamp at the use site or a surface that renders permanently empty with no error.
//!
//! One file rather than six, because the claim is about the SET of Phase-3 rows: the review found
//! that not one of them had a `validate` while every comparable Phase-1/2 row did.

use bough_kernel::Plugin;
use bough_plugin_commands::{CommandsConfig, CommandsPlugin};
use bough_plugin_residents::{ResidentsConfig, ResidentsPlugin};
use bough_plugin_tui_focus::{FocusConfig, FocusPlugin};
use bough_plugin_tui_search::{SearchConfig, SearchPlugin};
use bough_plugin_tui_shell::{ThemeName, TuiShellPlugin};
use bough_plugin_tui_strip::{StripConfig, StripPlugin};

fn shell() -> bough_plugin_tui_shell::TuiConfig {
    bough_plugin_tui_shell::test_config()
}

fn focus() -> FocusConfig {
    FocusConfig {
        max_rows: 2000,
        max_tool_lines: 200,
        page_lines: 20,
        expand_new_tools: false,
        show_reasoning: true,
    }
}

fn strip() -> StripConfig {
    StripConfig {
        width: 34,
        show_about: true,
        about_lines: 2,
        collapse_cols: 100,
        min_width: 22,
        max_width: 40,
        gutter: 1,
    }
}

fn search() -> SearchConfig {
    SearchConfig {
        height: 12,
        limit: 50,
        debounce_ms: 150,
    }
}

fn residents() -> ResidentsConfig {
    ResidentsConfig {
        bootstrap: vec!["sol".to_string()],
        traj_prefix: "lane/".to_string(),
        resume_all: true,
        catch_up: true,
    }
}

fn commands() -> CommandsConfig {
    CommandsConfig {
        prefix: '/',
        suggestions: true,
    }
}

#[test]
fn the_shipped_phase_three_configs_all_validate() {
    assert!(TuiShellPlugin::validate(&shell()).is_ok());
    assert!(FocusPlugin::validate(&focus()).is_ok());
    assert!(StripPlugin::validate(&strip()).is_ok());
    assert!(SearchPlugin::validate(&search()).is_ok());
    assert!(ResidentsPlugin::validate(&residents()).is_ok());
    assert!(CommandsPlugin::validate(&commands()).is_ok());
    // The theme is an enum, so the shipped one is valid by construction; naming it here keeps the
    // fixture honest about which config it is validating.
    assert_eq!(shell().theme, ThemeName::Dark);
}

/// The exact values the review named: each of them used to mount silently.
#[test]
fn a_nonsense_value_is_refused_at_load_by_every_phase_three_row() {
    // Clamped at the use site with `.max(1)` instead of refused.
    let mut c = shell();
    c.frame_ms = 0;
    assert!(TuiShellPlugin::validate(&c).is_err(), "frame_ms: 0");
    let mut c = shell();
    c.tick_ms = 0;
    assert!(TuiShellPlugin::validate(&c).is_err(), "tick_ms: 0");
    let mut c = shell();
    c.composer_max_lines = 0;
    assert!(
        TuiShellPlugin::validate(&c).is_err(),
        "composer_max_lines: 0"
    );
    let mut c = shell();
    c.search_pane = "  ".to_string();
    assert!(TuiShellPlugin::validate(&c).is_err(), "search_pane: blank");

    // `max_rows: 0` drained every step and issued `LIMIT 0`: a permanently empty trajectory.
    let mut c = focus();
    c.max_rows = 0;
    assert!(FocusPlugin::validate(&c).is_err(), "max_rows: 0");
    let mut c = focus();
    c.page_lines = 0;
    assert!(FocusPlugin::validate(&c).is_err(), "page_lines: 0");

    // `limit: 0` searches and always reports nothing.
    let mut c = search();
    c.limit = 0;
    assert!(SearchPlugin::validate(&c).is_err(), "limit: 0");
    let mut c = search();
    c.height = 0;
    assert!(SearchPlugin::validate(&c).is_err(), "height: 0");

    let mut c = strip();
    c.width = 0;
    assert!(StripPlugin::validate(&c).is_err(), "width: 0");
    let mut c = strip();
    c.about_lines = 0;
    assert!(
        StripPlugin::validate(&c).is_err(),
        "about_lines: 0 with show_about"
    );

    // `prefix: ' '` made every line beginning with a space a command.
    let mut c = commands();
    c.prefix = ' ';
    assert!(CommandsPlugin::validate(&c).is_err(), "prefix: ' '");
    let mut c = commands();
    c.prefix = 'x';
    assert!(
        CommandsPlugin::validate(&c).is_err(),
        "prefix: alphanumeric"
    );

    let mut c = residents();
    c.bootstrap = vec!["".to_string()];
    assert!(ResidentsPlugin::validate(&c).is_err(), "a blank agent name");
    let mut c = residents();
    c.resume_all = false;
    assert!(
        ResidentsPlugin::validate(&c).is_err(),
        "catch_up with no roster to wake"
    );
}
