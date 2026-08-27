//! V3 (WP-5, §17 Phase 8, §7): `kill -9` during a wake, against the real headless binary with a
//! pending `action/intent` row and a recording `gh` shim first on PATH. On restart the wake is
//! closed `interrupted`, nothing but the in-flight thought is lost, and the intent is reconciled —
//! never re-executed.
//!
//! Two variants matter: killed BEFORE the outward call, and killed AFTER it. The second is the one
//! that would re-execute if reconciliation guessed instead of looking the marker up in the world.

#[test]
#[ignore = "WP-5: fill in against the headless binary + actions-shim + the gh shim"]
fn a_killed_wake_reopens_closed_as_interrupted() {}

#[test]
#[ignore = "WP-5"]
fn only_the_in_flight_thought_is_missing_after_the_restart() {}

#[test]
#[ignore = "WP-5"]
fn every_unanswered_tool_call_gets_an_unknown_result() {}

#[test]
#[ignore = "WP-5"]
fn the_pending_intent_is_listed_after_the_restart_and_never_re_executed() {}

#[test]
#[ignore = "WP-5"]
fn a_kill_after_the_outward_call_still_yields_exactly_one_gh_invocation() {}
