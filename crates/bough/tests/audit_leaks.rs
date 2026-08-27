//! V5's leak column (WP-5, decision D-C6): the launcher prints no binding or listener counts, so
//! "nothing leaked" is asserted IN-PROCESS. For every `bough-base` row (plus the three new pane
//! rows): boot, record every binding and listener count, disable the row through the launcher's
//! live-recompose path, re-enable it, and require every count back at its pre-disable baseline.
//!
//! This is a stronger statement than two processes compared from the outside: it compares counts
//! across a live disable/re-enable in one address space.

#[test]
#[ignore = "WP-5: fill in over the bough-base rows"]
fn disabling_a_row_and_re_enabling_it_returns_every_binding_count_to_baseline() {}

#[test]
#[ignore = "WP-5"]
fn disabling_a_row_and_re_enabling_it_returns_every_listener_count_to_baseline() {}
