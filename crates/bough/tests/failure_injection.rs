//! V4 (WP-5, §17 Phase 8, §7, §12): a plugin fiber failing mid-wake ends THAT wake with reason
//! error and the loop continues; a FAILED row is reported and not retried; a panicking listener is
//! contained; and an llm failure arrives as a TERMINAL CHUNK rather than a thrown error.

#[test]
#[ignore = "WP-5: fill in with the fault-inject row mounted by patch"]
fn a_section_fault_ends_that_wake_with_reason_error() {}

#[test]
#[ignore = "WP-5"]
fn the_next_wake_after_a_faulted_one_completes() {}

#[test]
#[ignore = "WP-5"]
fn a_failed_row_is_reported_once_and_apply_is_never_called_again() {}

#[test]
#[ignore = "WP-5"]
fn a_failed_row_leaves_every_other_row_active() {}

#[test]
#[ignore = "WP-5"]
fn a_panicking_listener_is_contained_and_the_dispatch_continues() {}

#[test]
#[ignore = "WP-5"]
fn an_unmatched_replay_arrives_as_a_terminal_failed_chunk() {}
