//! V4's fan-out half (WP-5, §10): a spawn storm is held by the bounds, and every refusal REACHES
//! THE MODEL as a `tool/result` failure rather than a silent no-op.

#[test]
#[ignore = "WP-5: fill in with a scripted wake requesting 50 workers"]
fn fifty_spawns_in_one_wake_stop_at_the_per_wake_cap() {}

#[test]
#[ignore = "WP-5"]
fn in_flight_never_exceeds_max_in_flight_under_a_three_agent_storm() {}

#[test]
#[ignore = "WP-5"]
fn a_depth_four_spawn_is_refused() {}

#[test]
#[ignore = "WP-5"]
fn every_refusal_reaches_the_model_as_a_tool_result_failure() {}
