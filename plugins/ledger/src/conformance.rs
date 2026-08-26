//! Invariant: BOTH providers answer identically. This module is the provider-conformance suite:
//! one `async fn` per case over a [`Fixture`], plus the [`ledger_conformance!`] macro that expands
//! them into NAMED tests in a provider's `tests/` file (P1-D10) — so a provider cannot quietly
//! skip a case and a failure names the behaviour that broke, not "the suite".

use std::sync::Arc;

use bough_kernel::Context;
use parking_lot::Mutex;

use crate::step::Step;
use crate::LedgerHandle;

/// What each case is handed: a mounted provider, its context, and a tap on `ledger/step`.
pub struct Fixture {
    pub ledger: LedgerHandle,
    pub ctx: Context,
    pub tap: EventTap,
}

/// A recording listener on `ledger/step`. Cases await a RECEIPT here rather than sleeping, because
/// Phase 0's `emit` dispatch is spawned and never awaited (a Phase 1 deferral, not a Phase 1 fix).
#[derive(Clone, Default)]
pub struct EventTap {
    #[doc(hidden)]
    pub seen: Arc<Mutex<Vec<Arc<Step>>>>,
}

impl EventTap {
    /// Everything the tap has seen, in arrival order.
    pub fn seen(&self) -> Vec<Arc<Step>> {
        todo!("WP-1: EventTap::seen")
    }
    /// Wait until at least `n` steps have arrived, or time out. The receipt every event-observing
    /// case awaits.
    pub async fn wait_for(&self, n: usize) -> Vec<Arc<Step>> {
        todo!("WP-1: EventTap::wait_for")
    }
}

/// Conformance case: `a_committed_step_is_never_mutated`.
pub async fn a_committed_step_is_never_mutated(f: &Fixture) {
    todo!("WP-1: conformance::a_committed_step_is_never_mutated")
}

/// Conformance case: `superseding_twice_is_refused`.
pub async fn superseding_twice_is_refused(f: &Fixture) {
    todo!("WP-1: conformance::superseding_twice_is_refused")
}

/// Conformance case: `an_agent_row_can_be_updated_and_deleted`.
pub async fn an_agent_row_can_be_updated_and_deleted(f: &Fixture) {
    todo!("WP-1: conformance::an_agent_row_can_be_updated_and_deleted")
}

/// Conformance case: `evidence_without_cites_is_refused`.
pub async fn evidence_without_cites_is_refused(f: &Fixture) {
    todo!("WP-1: conformance::evidence_without_cites_is_refused")
}

/// Conformance case: `a_thought_never_promotes_to_evidence`.
pub async fn a_thought_never_promotes_to_evidence(f: &Fixture) {
    todo!("WP-1: conformance::a_thought_never_promotes_to_evidence")
}

/// Conformance case: `class_rule_refuses_a_thought_for_an_evidence_only_type`.
pub async fn class_rule_refuses_a_thought_for_an_evidence_only_type(f: &Fixture) {
    todo!("WP-1: conformance::class_rule_refuses_a_thought_for_an_evidence_only_type")
}

/// Conformance case: `step_refs_come_from_cites`.
pub async fn step_refs_come_from_cites(f: &Fixture) {
    todo!("WP-1: conformance::step_refs_come_from_cites")
}

/// Conformance case: `step_refs_come_from_body_refs`.
pub async fn step_refs_come_from_body_refs(f: &Fixture) {
    todo!("WP-1: conformance::step_refs_come_from_body_refs")
}

/// Conformance case: `step_refs_are_the_union_and_the_caller_cannot_set_them`.
pub async fn step_refs_are_the_union_and_the_caller_cannot_set_them(f: &Fixture) {
    todo!("WP-1: conformance::step_refs_are_the_union_and_the_caller_cannot_set_them")
}

/// Conformance case: `an_unregistered_type_is_refused_on_append`.
pub async fn an_unregistered_type_is_refused_on_append(f: &Fixture) {
    todo!("WP-1: conformance::an_unregistered_type_is_refused_on_append")
}

/// Conformance case: `an_unknown_type_is_refused_on_read`.
pub async fn an_unknown_type_is_refused_on_read(f: &Fixture) {
    todo!("WP-1: conformance::an_unknown_type_is_refused_on_read")
}

/// Conformance case: `an_unknown_ignorable_type_is_skipped_and_counted`.
pub async fn an_unknown_ignorable_type_is_skipped_and_counted(f: &Fixture) {
    todo!("WP-1: conformance::an_unknown_ignorable_type_is_skipped_and_counted")
}

/// Conformance case: `seq_starts_at_one_per_trajectory`.
pub async fn seq_starts_at_one_per_trajectory(f: &Fixture) {
    todo!("WP-1: conformance::seq_starts_at_one_per_trajectory")
}

/// Conformance case: `seq_has_no_gaps`.
pub async fn seq_has_no_gaps(f: &Fixture) {
    todo!("WP-1: conformance::seq_has_no_gaps")
}

/// Conformance case: `concurrent_appends_produce_a_contiguous_seq_run`.
pub async fn concurrent_appends_produce_a_contiguous_seq_run(f: &Fixture) {
    todo!("WP-1: conformance::concurrent_appends_produce_a_contiguous_seq_run")
}

/// Conformance case: `a_batch_append_is_one_contiguous_run`.
pub async fn a_batch_append_is_one_contiguous_run(f: &Fixture) {
    todo!("WP-1: conformance::a_batch_append_is_one_contiguous_run")
}

/// Conformance case: `head_seq_is_the_last_appended_seq`.
pub async fn head_seq_is_the_last_appended_seq(f: &Fixture) {
    todo!("WP-1: conformance::head_seq_is_the_last_appended_seq")
}

/// Conformance case: `tail_returns_the_newest_n_oldest_first`.
pub async fn tail_returns_the_newest_n_oldest_first(f: &Fixture) {
    todo!("WP-1: conformance::tail_returns_the_newest_n_oldest_first")
}

/// Conformance case: `steps_query_filters_by_kind_class_wake_and_refs`.
pub async fn steps_query_filters_by_kind_class_wake_and_refs(f: &Fixture) {
    todo!("WP-1: conformance::steps_query_filters_by_kind_class_wake_and_refs")
}

/// Conformance case: `live_pins_excludes_superseded_pins`.
pub async fn live_pins_excludes_superseded_pins(f: &Fixture) {
    todo!("WP-1: conformance::live_pins_excludes_superseded_pins")
}

/// Conformance case: `live_pins_ignores_age`.
pub async fn live_pins_ignores_age(f: &Fixture) {
    todo!("WP-1: conformance::live_pins_ignores_age")
}

/// Conformance case: `a_supersession_writes_nothing_onto_the_old_pin`.
pub async fn a_supersession_writes_nothing_onto_the_old_pin(f: &Fixture) {
    todo!("WP-1: conformance::a_supersession_writes_nothing_onto_the_old_pin")
}

/// Conformance case: `a_retired_pin_is_not_live`.
pub async fn a_retired_pin_is_not_live(f: &Fixture) {
    todo!("WP-1: conformance::a_retired_pin_is_not_live")
}

/// Conformance case: `unconsumed_mail_excludes_consumed_ranges`.
pub async fn unconsumed_mail_excludes_consumed_ranges(f: &Fixture) {
    todo!("WP-1: conformance::unconsumed_mail_excludes_consumed_ranges")
}

/// Conformance case: `unconsumed_mail_unions_consumed_sets_order_independently`.
pub async fn unconsumed_mail_unions_consumed_sets_order_independently(f: &Fixture) {
    todo!("WP-1: conformance::unconsumed_mail_unions_consumed_sets_order_independently")
}

/// Conformance case: `fork_at_a_closed_prefix_succeeds`.
pub async fn fork_at_a_closed_prefix_succeeds(f: &Fixture) {
    todo!("WP-1: conformance::fork_at_a_closed_prefix_succeeds")
}

/// Conformance case: `fork_inside_an_open_wake_is_refused_naming_the_wake`.
pub async fn fork_inside_an_open_wake_is_refused_naming_the_wake(f: &Fixture) {
    todo!("WP-1: conformance::fork_inside_an_open_wake_is_refused_naming_the_wake")
}

/// Conformance case: `a_refused_fork_writes_nothing`.
pub async fn a_refused_fork_writes_nothing(f: &Fixture) {
    todo!("WP-1: conformance::a_refused_fork_writes_nothing")
}

/// Conformance case: `a_fork_never_clips_the_prefix`.
pub async fn a_fork_never_clips_the_prefix(f: &Fixture) {
    todo!("WP-1: conformance::a_fork_never_clips_the_prefix")
}

/// Conformance case: `the_childs_first_step_is_the_end_seed_marker`.
pub async fn the_childs_first_step_is_the_end_seed_marker(f: &Fixture) {
    todo!("WP-1: conformance::the_childs_first_step_is_the_end_seed_marker")
}

/// Conformance case: `the_end_seed_carries_the_parent_and_at_seq`.
pub async fn the_end_seed_carries_the_parent_and_at_seq(f: &Fixture) {
    todo!("WP-1: conformance::the_end_seed_carries_the_parent_and_at_seq")
}

/// Conformance case: `connected_is_own_chain_plus_ancestry_plus_ref_matches`.
pub async fn connected_is_own_chain_plus_ancestry_plus_ref_matches(f: &Fixture) {
    todo!("WP-1: conformance::connected_is_own_chain_plus_ancestry_plus_ref_matches")
}

/// Conformance case: `connected_reads_the_agents_row_at_call_time`.
pub async fn connected_reads_the_agents_row_at_call_time(f: &Fixture) {
    todo!("WP-1: conformance::connected_reads_the_agents_row_at_call_time")
}

/// Conformance case: `a_late_linked_ref_includes_history_retroactively`.
pub async fn a_late_linked_ref_includes_history_retroactively(f: &Fixture) {
    todo!("WP-1: conformance::a_late_linked_ref_includes_history_retroactively")
}

/// Conformance case: `linking_a_ref_changes_no_step_row_hash`.
pub async fn linking_a_ref_changes_no_step_row_hash(f: &Fixture) {
    todo!("WP-1: conformance::linking_a_ref_changes_no_step_row_hash")
}

/// Conformance case: `connected_writes_nothing`.
pub async fn connected_writes_nothing(f: &Fixture) {
    todo!("WP-1: conformance::connected_writes_nothing")
}

/// Conformance case: `search_finds_a_step_in_another_trajectory`.
pub async fn search_finds_a_step_in_another_trajectory(f: &Fixture) {
    todo!("WP-1: conformance::search_finds_a_step_in_another_trajectory")
}

/// Conformance case: `a_hit_carries_its_cites`.
pub async fn a_hit_carries_its_cites(f: &Fixture) {
    todo!("WP-1: conformance::a_hit_carries_its_cites")
}

/// Conformance case: `search_respects_the_trajectory_filter`.
pub async fn search_respects_the_trajectory_filter(f: &Fixture) {
    todo!("WP-1: conformance::search_respects_the_trajectory_filter")
}

/// Conformance case: `search_ordering_is_deterministic`.
pub async fn search_ordering_is_deterministic(f: &Fixture) {
    todo!("WP-1: conformance::search_ordering_is_deterministic")
}

/// Conformance case: `a_sealed_rollup_is_readable_by_query`.
pub async fn a_sealed_rollup_is_readable_by_query(f: &Fixture) {
    todo!("WP-1: conformance::a_sealed_rollup_is_readable_by_query")
}

/// Conformance case: `an_action_intent_then_done_updates_the_journal_row`.
pub async fn an_action_intent_then_done_updates_the_journal_row(f: &Fixture) {
    todo!("WP-1: conformance::an_action_intent_then_done_updates_the_journal_row")
}

/// Conformance case: `trajectory_view_returns_steps_edges_and_rollups`.
pub async fn trajectory_view_returns_steps_edges_and_rollups(f: &Fixture) {
    todo!("WP-1: conformance::trajectory_view_returns_steps_edges_and_rollups")
}

/// Expands the whole conformance suite into named `#[tokio::test]`s in a provider's test file.
///
/// ```ignore
/// bough_plugin_ledger::ledger_conformance!(|| async { my_fixture().await });
/// ```
#[macro_export]
macro_rules! ledger_conformance {
    ($fixture:expr) => {
        $crate::ledger_conformance_cases!($fixture;
            a_committed_step_is_never_mutated,
            superseding_twice_is_refused,
            an_agent_row_can_be_updated_and_deleted,
            evidence_without_cites_is_refused,
            a_thought_never_promotes_to_evidence,
            class_rule_refuses_a_thought_for_an_evidence_only_type,
            step_refs_come_from_cites,
            step_refs_come_from_body_refs,
            step_refs_are_the_union_and_the_caller_cannot_set_them,
            an_unregistered_type_is_refused_on_append,
            an_unknown_type_is_refused_on_read,
            an_unknown_ignorable_type_is_skipped_and_counted,
            seq_starts_at_one_per_trajectory,
            seq_has_no_gaps,
            concurrent_appends_produce_a_contiguous_seq_run,
            a_batch_append_is_one_contiguous_run,
            head_seq_is_the_last_appended_seq,
            tail_returns_the_newest_n_oldest_first,
            steps_query_filters_by_kind_class_wake_and_refs,
            live_pins_excludes_superseded_pins,
            live_pins_ignores_age,
            a_supersession_writes_nothing_onto_the_old_pin,
            a_retired_pin_is_not_live,
            unconsumed_mail_excludes_consumed_ranges,
            unconsumed_mail_unions_consumed_sets_order_independently,
            fork_at_a_closed_prefix_succeeds,
            fork_inside_an_open_wake_is_refused_naming_the_wake,
            a_refused_fork_writes_nothing,
            a_fork_never_clips_the_prefix,
            the_childs_first_step_is_the_end_seed_marker,
            the_end_seed_carries_the_parent_and_at_seq,
            connected_is_own_chain_plus_ancestry_plus_ref_matches,
            connected_reads_the_agents_row_at_call_time,
            a_late_linked_ref_includes_history_retroactively,
            linking_a_ref_changes_no_step_row_hash,
            connected_writes_nothing,
            search_finds_a_step_in_another_trajectory,
            a_hit_carries_its_cites,
            search_respects_the_trajectory_filter,
            search_ordering_is_deterministic,
            a_sealed_rollup_is_readable_by_query,
            an_action_intent_then_done_updates_the_journal_row,
            trajectory_view_returns_steps_edges_and_rollups,
        );
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! ledger_conformance_cases {
    ($fixture:expr; $($case:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                let f = ($fixture)().await;
                $crate::conformance::$case(&f).await;
            }
        )*
    };
}
