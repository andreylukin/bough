//! WP-4: the two facts a mounted fault row must have — a FAILED fiber from an `Apply` fault, and a
//! section fault that fails the SECTION rather than `assemble` itself.

#[test]
#[ignore = "WP-4: fill in when the sites are armed"]
fn an_apply_fault_leaves_the_fiber_failed_and_apply_ran_once() {}

#[test]
#[ignore = "WP-4: fill in when the sites are armed"]
fn a_projection_section_fault_returns_err_from_the_section_and_not_from_assemble_itself() {}
