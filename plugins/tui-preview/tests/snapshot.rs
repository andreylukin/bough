//! V1's supporting half (WP-1): a snapshot at one `as_of` is byte-identical taken twice, and
//! ignores every row above that `as_of`. The primary V1 assertion — the pane's bytes against the
//! request the loop actually sent — is `crates/bough/tests/preview_bytes.rs` (WP-7).

#[test]
#[ignore = "WP-1: fill in when snapshot() lands"]
fn two_snapshots_at_one_seq_are_byte_identical() {}

#[test]
#[ignore = "WP-1: fill in when snapshot() lands"]
fn a_snapshot_at_a_seq_ignores_every_row_above_it() {}
