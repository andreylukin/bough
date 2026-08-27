//! WP-4: the two Provider-level facts V3 leans on — one execute is one `gh` invocation, and a
//! failing `gh` still closes the journal row rather than leaving an intent with no done.

#[test]
#[ignore = "WP-4: fill in when GhShimProvider::execute lands"]
fn one_execute_is_one_gh_invocation() {}

#[test]
#[ignore = "WP-4: fill in when GhShimProvider::execute lands"]
fn a_failing_gh_marks_the_row_failed_and_still_writes_action_done() {}
