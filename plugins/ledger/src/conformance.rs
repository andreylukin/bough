//! Invariant: BOTH providers answer identically. This module is the provider-conformance suite:
//! one `async fn` per case over a [`Fixture`], plus the [`ledger_conformance!`] macro that expands
//! them into NAMED tests in a provider's `tests/` file (P1-D10) — so a provider cannot quietly
//! skip a case and a failure names the behaviour that broke, not "the suite".

use std::sync::Arc;

use bough_kernel::Context;
use parking_lot::Mutex;

use chrono::{DateTime, TimeZone, Utc};

use crate::id::{
    ActionId, AgentName, IdemKey, Ref, RollupId, Seq, StepId, StepType, TrajId, WakeId,
};
use crate::query::{ActionQuery, Fork, HashScope, Order, RollupQuery, SearchQuery, StepQuery};
use crate::rows::{ActionStatus, AgentRow, EdgeKind, NewAction, NewRollup, RollupKind};
use crate::step::{Append, Cite, Class, SeqRange, Step};
use crate::types::StepTypeDef;
use crate::{LedgerError, LedgerHandle};

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
        self.seen.lock().clone()
    }
    /// Wait until at least `n` steps have arrived, or time out. The receipt every event-observing
    /// case awaits.
    pub async fn wait_for(&self, n: usize) -> Vec<Arc<Step>> {
        // A poll, not a sleep: `emit` dispatch is spawned, so the only honest way to observe it is
        // to wait for the receipt and give up loudly rather than assume a duration was enough.
        for _ in 0..400 {
            let seen = self.seen();
            if seen.len() >= n {
                return seen;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!(
            "timed out waiting for {n} `ledger/step` events; saw {}",
            self.seen().len()
        )
    }
}

// ---- helpers ------------------------------------------------------------------------------
//
// Every case builds its own trajectories, so a fixture is never shared between cases and no case
// depends on another's rows.

/// A fixed clock. Determinism is the whole value of this suite: two providers must answer
/// identically, and `Utc::now()` in a fixture would put a wall clock in the comparison.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .expect("a valid instant")
}

fn traj(name: &str) -> TrajId {
    TrajId::new(name)
}

fn wake(name: &str) -> WakeId {
    WakeId::new(name)
}

fn cite(r: &str) -> Cite {
    Cite {
        r#ref: Ref::new(r),
        url: None,
    }
}

/// A thought append with no cites.
fn thought(t: &TrajId, w: &WakeId, kind: &str, body: serde_json::Value) -> Append {
    Append {
        traj: t.clone(),
        wake: w.clone(),
        kind: StepType::new(kind),
        class: Class::Thought,
        body,
        cites: vec![],
        at: at(0),
        id: None,
    }
}

/// An evidence append. Evidence requires cites (§3), so the helper insists on them too.
fn evidence(
    t: &TrajId,
    w: &WakeId,
    kind: &str,
    body: serde_json::Value,
    cites: Vec<Cite>,
) -> Append {
    assert!(!cites.is_empty(), "evidence requires citations");
    Append {
        class: Class::Evidence,
        cites,
        ..thought(t, w, kind, body)
    }
}

fn wake_start() -> serde_json::Value {
    serde_json::json!({ "urgency": "immediate", "trigger": null, "claimed": [] })
}

fn wake_end(consumed: &[(u64, u64)]) -> serde_json::Value {
    let ranges: Vec<serde_json::Value> = consumed
        .iter()
        .map(|(f, t)| serde_json::json!({ "from": f, "to": t }))
        .collect();
    serde_json::json!({ "reason": "completed", "cause": null, "consumed": ranges })
}

fn pin_set(title: &str, text: &str, supersedes: &[&StepId]) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "text": text,
        "supersedes": supersedes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    })
}

fn mail(subject: &str, summary: &str) -> serde_json::Value {
    serde_json::json!({
        "class": "wake", "from": "gh:o/r", "subject": subject, "summary": summary, "refs": []
    })
}

/// Append one step, insisting it is accepted. Most cases care about the row, not the call.
async fn ok(f: &Fixture, req: Append) -> Step {
    let kind = req.kind.clone();
    f.ledger
        .0
        .append(req)
        .await
        .unwrap_or_else(|e| panic!("append of `{kind}` was refused: {e}"))
}

/// Append a closed wake carrying `inner` steps, and return them. The enclosure invariant is a
/// standing rule, so a fixture that opens a wake closes it.
async fn closed_wake(f: &Fixture, t: &TrajId, w: &WakeId, inner: Vec<Append>) -> Vec<Step> {
    ok(f, thought(t, w, "wake/start", wake_start())).await;
    let mut out = Vec::new();
    for req in inner {
        out.push(ok(f, req).await);
    }
    ok(f, thought(t, w, "wake/end", wake_end(&[]))).await;
    out
}

/// Every step of a trajectory, oldest first.
async fn all_steps(f: &Fixture, t: &TrajId) -> Vec<Step> {
    f.ledger
        .0
        .steps(&StepQuery {
            trajs: vec![t.clone()],
            ..Default::default()
        })
        .await
        .expect("a step query over a trajectory")
}

fn seal(id: &str, t: &TrajId) -> NewRollup {
    NewRollup {
        id: Some(RollupId::new(id)),
        traj: t.clone(),
        kind: RollupKind::Tier,
        tier: 1,
        from_seq: Seq(1),
        to_seq: Seq(2),
        src_trajs: vec![t.clone()],
        body: serde_json::json!({ "themes": ["a theme"] }),
        notable_refs: [Ref::new("gh:o/r#1")].into_iter().collect(),
        prompt_ver: "p1".to_string(),
        sealed_at: at(10),
    }
}

// ---- cases --------------------------------------------------------------------------------

/// Conformance case: `a_committed_step_is_never_mutated`.
pub async fn a_committed_step_is_never_mutated(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let first = ok(
        f,
        thought(&t, &w, "pin/set", pin_set("keep", "verbatim", &[])),
    )
    .await;
    let before = f
        .ledger
        .0
        .row_hashes(HashScope::Steps)
        .await
        .expect("row hashes");
    // Everything the API offers afterwards: more appends, a supersession, an agent write.
    for i in 0..3 {
        ok(
            f,
            thought(&t, &w, "step/start", serde_json::json!({ "index": i })),
        )
        .await;
    }
    f.ledger.0.seal_rollup(seal("r1", &t)).await.expect("seal");
    f.ledger.0.seal_rollup(seal("r2", &t)).await.expect("seal");
    f.ledger
        .0
        .supersede_rollup(&RollupId::new("r1"), &RollupId::new("r2"))
        .await
        .expect("the one permitted write to a sealed row");

    let again = f
        .ledger
        .0
        .step(&first.id)
        .await
        .expect("read back")
        .expect("the step is there");
    assert_eq!(again, first, "a committed step is never mutated");
    let after = f
        .ledger
        .0
        .row_hashes(HashScope::Steps)
        .await
        .expect("row hashes");
    for row in &before {
        let now = after
            .iter()
            .find(|r| r.table == row.table && r.id == row.id)
            .unwrap_or_else(|| panic!("row `{}` of `{}` disappeared", row.id, row.table));
        assert_eq!(now.hash, row.hash, "row `{}` changed content hash", row.id);
    }
}

/// Conformance case: `superseding_twice_is_refused`.
pub async fn superseding_twice_is_refused(f: &Fixture) {
    let t = traj("t1");
    for id in ["r1", "r2", "r3"] {
        f.ledger.0.seal_rollup(seal(id, &t)).await.expect("seal");
    }
    f.ledger
        .0
        .supersede_rollup(&RollupId::new("r1"), &RollupId::new("r2"))
        .await
        .expect("the first supersession is the one permitted write");
    let err = f
        .ledger
        .0
        .supersede_rollup(&RollupId::new("r1"), &RollupId::new("r3"))
        .await
        .expect_err("superseded_by is set ONCE");
    match err {
        LedgerError::AlreadySuperseded(old, new) => {
            assert_eq!(old.as_str(), "r1");
            assert_eq!(
                new.as_str(),
                "r2",
                "the error names the standing supersession"
            );
        }
        other => panic!("wrong refusal: {other}"),
    }
    let rows = f
        .ledger
        .0
        .rollups(&RollupQuery {
            include_superseded: true,
            ..Default::default()
        })
        .await
        .expect("rollup query");
    let r1 = rows
        .iter()
        .find(|r| r.id.as_str() == "r1")
        .expect("r1 is still there");
    assert_eq!(r1.superseded_by.as_ref().map(|r| r.as_str()), Some("r2"));
}

/// Conformance case: `an_agent_row_can_be_updated_and_deleted`.
pub async fn an_agent_row_can_be_updated_and_deleted(f: &Fixture) {
    // §3 exempts `agents` from append-only in as many words: it is MUTABLE CONFIG.
    let name = AgentName::new("sol");
    let row = AgentRow {
        name: name.clone(),
        traj: traj("t1"),
        routing_refs: [Ref::new("gh:o/r")].into_iter().collect(),
        wake_classes: ["answer".to_string()].into_iter().collect(),
        model_override: None,
        tick_floor: None,
        digest_rollup: None,
    };
    f.ledger.0.put_agent(row.clone()).await.expect("put");
    assert_eq!(
        f.ledger.0.agent(&name).await.expect("read").as_ref(),
        Some(&row)
    );

    let updated = AgentRow {
        routing_refs: [Ref::new("gh:o/r"), Ref::new("linear:ENG")]
            .into_iter()
            .collect(),
        model_override: Some("terra".to_string()),
        ..row
    };
    f.ledger.0.put_agent(updated.clone()).await.expect("update");
    assert_eq!(
        f.ledger.0.agent(&name).await.expect("read").as_ref(),
        Some(&updated)
    );
    assert_eq!(f.ledger.0.agents().await.expect("list").len(), 1);

    f.ledger.0.delete_agent(&name).await.expect("delete");
    assert_eq!(f.ledger.0.agent(&name).await.expect("read"), None);
    assert!(f.ledger.0.agents().await.expect("list").is_empty());
}

/// Conformance case: `evidence_without_cites_is_refused`.
pub async fn evidence_without_cites_is_refused(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let req = Append {
        class: Class::Evidence,
        ..thought(&t, &w, "mail/delivered", mail("s", "x"))
    };
    let err = f
        .ledger
        .0
        .append(req)
        .await
        .expect_err("evidence requires citations");
    assert!(
        matches!(err, LedgerError::EvidenceWithoutCites { .. }),
        "wrong refusal: {err}"
    );
    // A refused append writes nothing.
    assert_eq!(f.ledger.0.head_seq(&t).await.expect("head"), None);
}

/// Conformance case: `a_thought_never_promotes_to_evidence`.
pub async fn a_thought_never_promotes_to_evidence(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    // A thought MAY cite. Citing is not what makes a row evidence; the class is.
    let step = ok(
        f,
        Append {
            cites: vec![cite("gh:o/r#1")],
            ..thought(&t, &w, "pin/set", pin_set("t", "x", &[]))
        },
    )
    .await;
    assert_eq!(step.class, Class::Thought);
    let back = f
        .ledger
        .0
        .step(&step.id)
        .await
        .expect("read")
        .expect("there");
    assert_eq!(
        back.class,
        Class::Thought,
        "a thought never promotes to evidence"
    );
    // And a class filter agrees.
    let evidence_rows = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![t.clone()],
            class: Some(Class::Evidence),
            ..Default::default()
        })
        .await
        .expect("query");
    assert!(evidence_rows.is_empty());
}

/// Conformance case: `class_rule_refuses_a_thought_for_an_evidence_only_type`.
pub async fn class_rule_refuses_a_thought_for_an_evidence_only_type(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let err = f
        .ledger
        .0
        .append(thought(&t, &w, "mail/delivered", mail("s", "x")))
        .await
        .expect_err("`mail/delivered` is evidence-only");
    match err {
        LedgerError::ClassRuleViolated {
            kind,
            expected,
            got,
        } => {
            assert_eq!(kind.as_str(), "mail/delivered");
            assert_eq!(expected, "evidence");
            assert_eq!(got, "thought");
        }
        other => panic!("wrong refusal: {other}"),
    }
}

/// Conformance case: `step_refs_come_from_cites`.
pub async fn step_refs_come_from_cites(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let step = ok(
        f,
        evidence(
            &t,
            &w,
            "mail/delivered",
            mail("s", "x"),
            vec![cite("gh:o/r#12"), cite("linear:ENG-1")],
        ),
    )
    .await;
    assert!(step.refs.contains(&Ref::new("gh:o/r#12")));
    assert!(step.refs.contains(&Ref::new("linear:ENG-1")));
    // And the derived index is what the query matches on.
    let hits = f
        .ledger
        .0
        .steps(&StepQuery {
            refs: vec![Ref::new("linear:ENG-1")],
            ..Default::default()
        })
        .await
        .expect("ref query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, step.id);
}

/// Conformance case: `step_refs_come_from_body_refs`.
pub async fn step_refs_come_from_body_refs(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    // `mail/delivered.from` is a Ref-typed field and `refs` an array — both live in the body, at
    // different depths, and both must reach `step_refs`.
    let step = ok(
        f,
        evidence(
            &t,
            &w,
            "mail/delivered",
            serde_json::json!({
                "class": "ordinary", "from": "gh:o/r", "subject": "s", "summary": "x",
                "refs": ["deep:1", "deep:2"]
            }),
            vec![cite("cited:1")],
        ),
    )
    .await;
    assert!(step.refs.contains(&Ref::new("deep:1")));
    assert!(step.refs.contains(&Ref::new("deep:2")));
    let hits = f
        .ledger
        .0
        .steps(&StepQuery {
            refs: vec![Ref::new("deep:2")],
            ..Default::default()
        })
        .await
        .expect("ref query");
    assert_eq!(hits.len(), 1, "a body ref is matchable");
}

/// Conformance case: `step_refs_are_the_union_and_the_caller_cannot_set_them`.
pub async fn step_refs_are_the_union_and_the_caller_cannot_set_them(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let body = serde_json::json!({
        "class": "ordinary", "from": "gh:o/r", "subject": "s", "summary": "x",
        "refs": ["b:1"]
    });
    let cites = vec![cite("c:1"), cite("c:2")];
    let step = ok(
        f,
        evidence(&t, &w, "mail/delivered", body.clone(), cites.clone()),
    )
    .await;
    // `Append` has no `refs` field at all: derivation is the ONLY way a ref enters the index, and
    // it is this crate's function, which is why the two providers cannot diverge (§3).
    let expected = crate::refs::derive_step_refs(&cites, &body);
    assert_eq!(*step.refs, expected);
    let back = f
        .ledger
        .0
        .step(&step.id)
        .await
        .expect("read")
        .expect("there");
    assert_eq!(*back.refs, expected, "the stored index is the derived one");
}

/// Conformance case: `an_unregistered_type_is_refused_on_append`.
pub async fn an_unregistered_type_is_refused_on_append(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let err = f
        .ledger
        .0
        .append(thought(
            &t,
            &w,
            "nobody/declared-this",
            serde_json::json!({}),
        ))
        .await
        .expect_err("an unregistered type cannot be appended");
    match err {
        LedgerError::UnknownStepTypeOnAppend { kind } => {
            assert_eq!(kind.as_str(), "nobody/declared-this")
        }
        other => panic!("wrong refusal: {other}"),
    }
    assert_eq!(f.ledger.0.head_seq(&t).await.expect("head"), None);
}

/// Conformance case: `an_unknown_type_is_refused_on_read`.
pub async fn an_unknown_type_is_refused_on_read(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let token = f
        .ledger
        .0
        .register_step_type(StepTypeDef::of::<ProbeNote>("probe/opaque", "probe"))
        .expect("a fresh type registers");
    ok(
        f,
        thought(
            &t,
            &w,
            "probe/opaque",
            serde_json::json!({ "text": "hello" }),
        ),
    )
    .await;
    // The row outlives the binary that understood it — exactly the situation §3 legislates.
    token.unregister();

    let err = all_steps_err(f, &t).await;
    match err {
        LedgerError::UnknownStepTypeOnRead { kind, traj: tr, .. } => {
            assert_eq!(kind.as_str(), "probe/opaque");
            assert_eq!(tr, t);
        }
        other => panic!("wrong refusal: {other}"),
    }
}

/// Conformance case: `an_unknown_ignorable_type_is_skipped_and_counted`.
pub async fn an_unknown_ignorable_type_is_skipped_and_counted(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let token = f
        .ledger
        .0
        .register_step_type(StepTypeDef::of::<ProbeNote>("probe/chatter", "probe").ignorable(true))
        .expect("a fresh type registers");
    ok(
        f,
        thought(
            &t,
            &w,
            "probe/chatter",
            serde_json::json!({ "text": "skip me" }),
        ),
    )
    .await;
    let kept = ok(f, thought(&t, &w, "pin/set", pin_set("t", "x", &[]))).await;
    let before = f.ledger.0.skipped_ignorable();
    token.unregister();

    let steps = all_steps(f, &t).await;
    assert_eq!(
        steps.len(),
        1,
        "the ignorable unknown row is skipped, not refused"
    );
    assert_eq!(steps[0].id, kept.id);
    assert!(
        f.ledger.0.skipped_ignorable() > before,
        "a skipped row is COUNTED, so a reader can tell it was there"
    );
}

/// Conformance case: `seq_starts_at_one_per_trajectory`.
pub async fn seq_starts_at_one_per_trajectory(f: &Fixture) {
    let w = wake("w1");
    for name in ["t1", "t2"] {
        let t = traj(name);
        let first = ok(f, thought(&t, &w, "pin/set", pin_set("t", "x", &[]))).await;
        assert_eq!(first.seq, Seq(1), "seq is per trajectory and 1-based");
    }
}

/// Conformance case: `seq_has_no_gaps`.
pub async fn seq_has_no_gaps(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let mut seqs = Vec::new();
    for i in 0..6u32 {
        seqs.push(
            ok(
                f,
                thought(&t, &w, "step/start", serde_json::json!({ "index": i })),
            )
            .await
            .seq,
        );
    }
    assert_eq!(seqs, (1..=6).map(Seq).collect::<Vec<_>>());
    // A refused append does not burn a seq either.
    let _ = f
        .ledger
        .0
        .append(thought(
            &t,
            &w,
            "step/start",
            serde_json::json!({ "index": "no" }),
        ))
        .await
        .expect_err("a bad body is refused");
    assert_eq!(
        ok(
            f,
            thought(&t, &w, "step/start", serde_json::json!({ "index": 6 }))
        )
        .await
        .seq,
        Seq(7)
    );
}

/// Conformance case: `concurrent_appends_produce_a_contiguous_seq_run`.
pub async fn concurrent_appends_produce_a_contiguous_seq_run(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let mut tasks = Vec::new();
    for i in 0..16u32 {
        let ledger = f.ledger.clone();
        let req = thought(&t, &w, "step/start", serde_json::json!({ "index": i }));
        tasks.push(tokio::spawn(async move { ledger.0.append(req).await }));
    }
    let mut seqs = Vec::new();
    for task in tasks {
        seqs.push(task.await.expect("task").expect("append").seq.0);
    }
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        (1..=16).collect::<Vec<u64>>(),
        "seq is allocated by the SINGLE writer inside the commit: no collision, no gap"
    );
}

/// Conformance case: `a_batch_append_is_one_contiguous_run`.
pub async fn a_batch_append_is_one_contiguous_run(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    ok(
        f,
        thought(&t, &w, "step/start", serde_json::json!({ "index": 0 })),
    )
    .await;
    let batch: Vec<Append> = (1..4u32)
        .map(|i| thought(&t, &w, "step/start", serde_json::json!({ "index": i })))
        .collect();
    let steps = f.ledger.0.append_batch(batch).await.expect("batch append");
    assert_eq!(
        steps.iter().map(|s| s.seq.0).collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert_eq!(f.ledger.0.head_seq(&t).await.expect("head"), Some(Seq(4)));
}

/// Conformance case: `head_seq_is_the_last_appended_seq`.
pub async fn head_seq_is_the_last_appended_seq(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    assert_eq!(
        f.ledger.0.head_seq(&t).await.expect("head"),
        None,
        "an empty trajectory has no head"
    );
    for i in 0..3u32 {
        let step = ok(
            f,
            thought(&t, &w, "step/start", serde_json::json!({ "index": i })),
        )
        .await;
        assert_eq!(f.ledger.0.head_seq(&t).await.expect("head"), Some(step.seq));
    }
}

/// Conformance case: `tail_returns_the_newest_n_oldest_first`.
pub async fn tail_returns_the_newest_n_oldest_first(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    for i in 0..10u32 {
        ok(
            f,
            thought(&t, &w, "step/start", serde_json::json!({ "index": i })),
        )
        .await;
    }
    let tail = f.ledger.0.tail(&t, 3).await.expect("tail");
    assert_eq!(
        tail.iter().map(|s| s.seq.0).collect::<Vec<_>>(),
        vec![8, 9, 10],
        "the tail is the NEWEST n, rendered oldest first"
    );
    // Asking for more than there is returns everything, not an error.
    assert_eq!(f.ledger.0.tail(&t, 50).await.expect("tail").len(), 10);
    assert!(f
        .ledger
        .0
        .tail(&traj("nothing-here"), 5)
        .await
        .expect("tail")
        .is_empty());
}

/// Conformance case: `steps_query_filters_by_kind_class_wake_and_refs`.
pub async fn steps_query_filters_by_kind_class_wake_and_refs(f: &Fixture) {
    let t = traj("t1");
    let other = traj("t2");
    let w1 = wake("w1");
    let w2 = wake("w2");
    let pin = ok(f, thought(&t, &w1, "pin/set", pin_set("p", "x", &[]))).await;
    let mail_step = ok(
        f,
        evidence(
            &t,
            &w2,
            "mail/delivered",
            mail("s", "x"),
            vec![cite("gh:o/r#5")],
        ),
    )
    .await;
    ok(
        f,
        thought(&other, &w1, "pin/set", pin_set("elsewhere", "x", &[])),
    )
    .await;

    let by_traj = all_steps(f, &t).await;
    assert_eq!(by_traj.len(), 2);
    assert_eq!(by_traj[0].seq, Seq(1), "SeqAsc is the default order");

    let by_kind = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![t.clone()],
            kinds: vec![StepType::new("pin/set")],
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(
        by_kind.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![pin.id.clone()]
    );

    let by_class = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![t.clone()],
            class: Some(Class::Evidence),
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(
        by_class.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![mail_step.id.clone()]
    );

    let by_wake = f
        .ledger
        .0
        .steps(&StepQuery {
            wake: Some(w2.clone()),
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(by_wake.len(), 1);

    let by_ref = f
        .ledger
        .0
        .steps(&StepQuery {
            refs: vec![Ref::new("gh:o/r#5")],
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(by_ref.len(), 1);

    let desc = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![t.clone()],
            order: Order::SeqDesc,
            limit: Some(1),
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(desc.len(), 1);
    assert_eq!(desc[0].seq, Seq(2));

    let by_range = f
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![t.clone()],
            after: Some(Seq(1)),
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(
        by_range.iter().map(|s| s.seq).collect::<Vec<_>>(),
        vec![Seq(2)]
    );
}

/// Conformance case: `live_pins_excludes_superseded_pins`.
pub async fn live_pins_excludes_superseded_pins(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let old = ok(
        f,
        thought(&t, &w, "pin/set", pin_set("budget", "under 50ms", &[])),
    )
    .await;
    let new = ok(
        f,
        thought(
            &t,
            &w,
            "pin/set",
            pin_set("budget", "under 40ms", &[&old.id]),
        ),
    )
    .await;
    let pins = f
        .ledger
        .0
        .live_pins(std::slice::from_ref(&t))
        .await
        .expect("live pins");
    assert_eq!(
        pins.len(),
        1,
        "the superseded pin is gone; the superseding one stands"
    );
    assert_eq!(pins[0].step, new.id);
    assert_eq!(pins[0].text, "under 40ms");
    assert_eq!(pins[0].title, "budget");
}

/// Conformance case: `live_pins_ignores_age`.
pub async fn live_pins_ignores_age(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let ancient = ok(
        f,
        thought(&t, &w, "pin/set", pin_set("first", "oldest fact", &[])),
    )
    .await;
    assert_eq!(ancient.seq, Seq(1));
    for i in 0..100u32 {
        ok(
            f,
            thought(&t, &w, "step/start", serde_json::json!({ "index": i })),
        )
        .await;
    }
    let pins = f
        .ledger
        .0
        .live_pins(std::slice::from_ref(&t))
        .await
        .expect("live pins");
    // §3: a pin is never demoted into tiers and never expired. Age is not a criterion.
    assert_eq!(
        pins.iter().map(|p| p.step.clone()).collect::<Vec<_>>(),
        vec![ancient.id]
    );
}

/// Conformance case: `a_supersession_writes_nothing_onto_the_old_pin`.
pub async fn a_supersession_writes_nothing_onto_the_old_pin(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let old = ok(
        f,
        thought(&t, &w, "pin/set", pin_set("budget", "50ms", &[])),
    )
    .await;
    let before = f
        .ledger
        .0
        .row_hashes(HashScope::Steps)
        .await
        .expect("hashes");
    ok(
        f,
        thought(&t, &w, "pin/set", pin_set("budget", "40ms", &[&old.id])),
    )
    .await;

    let again = f
        .ledger
        .0
        .step(&old.id)
        .await
        .expect("read")
        .expect("there");
    assert_eq!(
        again, old,
        "supersession is an APPENDED marker, not an edit"
    );
    let after = f
        .ledger
        .0
        .row_hashes(HashScope::Steps)
        .await
        .expect("hashes");
    let old_row = before
        .iter()
        .find(|r| r.id == old.id.as_str())
        .expect("the old pin's row hash");
    let now = after
        .iter()
        .find(|r| r.id == old.id.as_str())
        .expect("still there");
    assert_eq!(now.hash, old_row.hash);
}

/// Conformance case: `a_retired_pin_is_not_live`.
pub async fn a_retired_pin_is_not_live(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let a = ok(f, thought(&t, &w, "pin/set", pin_set("keep", "x", &[]))).await;
    let b = ok(f, thought(&t, &w, "pin/set", pin_set("drop", "y", &[]))).await;
    ok(
        f,
        thought(
            &t,
            &w,
            "pin/retire",
            serde_json::json!({ "retires": [b.id.as_str()], "reason": "withdrawn" }),
        ),
    )
    .await;
    let pins = f
        .ledger
        .0
        .live_pins(std::slice::from_ref(&t))
        .await
        .expect("live pins");
    assert_eq!(
        pins.iter().map(|p| p.step.clone()).collect::<Vec<_>>(),
        vec![a.id]
    );
}

/// Conformance case: `unconsumed_mail_excludes_consumed_ranges`.
pub async fn unconsumed_mail_excludes_consumed_ranges(f: &Fixture) {
    let t = traj("t1");
    let w1 = wake("w1");
    let w2 = wake("w2");
    let first = ok(
        f,
        evidence(
            &t,
            &w1,
            "mail/delivered",
            mail("one", "x"),
            vec![cite("m:1")],
        ),
    )
    .await;
    let second = ok(
        f,
        evidence(
            &t,
            &w1,
            "mail/delivered",
            mail("two", "x"),
            vec![cite("m:2")],
        ),
    )
    .await;
    assert_eq!((first.seq, second.seq), (Seq(1), Seq(2)));

    let unread = f
        .ledger
        .0
        .unconsumed_mail(&t)
        .await
        .expect("unconsumed mail");
    assert_eq!(unread.len(), 2, "nothing has been consumed yet");

    // One wake consumes seq 1 only.
    ok(f, thought(&t, &w2, "wake/start", wake_start())).await;
    ok(f, thought(&t, &w2, "wake/end", wake_end(&[(1, 1)]))).await;
    let unread = f
        .ledger
        .0
        .unconsumed_mail(&t)
        .await
        .expect("unconsumed mail");
    assert_eq!(
        unread.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![second.id]
    );
}

/// Conformance case: `unconsumed_mail_unions_consumed_sets_order_independently`.
pub async fn unconsumed_mail_unions_consumed_sets_order_independently(f: &Fixture) {
    let t = traj("t1");
    let w = wake("w1");
    let mut mails = Vec::new();
    for i in 0..4 {
        mails.push(
            ok(
                f,
                evidence(
                    &t,
                    &w,
                    "mail/delivered",
                    mail(&format!("m{i}"), "x"),
                    vec![cite("m:1")],
                ),
            )
            .await,
        );
    }
    // Two wakes, closing out of seq order and with overlapping ranges: the union is a set, so
    // neither the order nor the overlap can change the answer (§5).
    ok(f, thought(&t, &wake("w3"), "wake/start", wake_start())).await;
    ok(f, thought(&t, &wake("w3"), "wake/end", wake_end(&[(3, 4)]))).await;
    ok(f, thought(&t, &wake("w2"), "wake/start", wake_start())).await;
    ok(
        f,
        thought(&t, &wake("w2"), "wake/end", wake_end(&[(1, 1), (2, 3)])),
    )
    .await;

    let unread = f
        .ledger
        .0
        .unconsumed_mail(&t)
        .await
        .expect("unconsumed mail");
    assert!(
        unread.is_empty(),
        "1-1 ∪ 2-3 ∪ 3-4 covers every mail row: {unread:?}"
    );
    assert_eq!(
        SeqRange::union(&[
            SeqRange {
                from: Seq(3),
                to: Seq(4)
            },
            SeqRange {
                from: Seq(1),
                to: Seq(1)
            },
            SeqRange {
                from: Seq(2),
                to: Seq(3)
            },
        ]),
        vec![SeqRange {
            from: Seq(1),
            to: Seq(4)
        }],
        "the union the store computes is this crate's, so the two providers agree"
    );
    let _ = mails;
}

/// Conformance case: `fork_at_a_closed_prefix_succeeds`.
pub async fn fork_at_a_closed_prefix_succeeds(f: &Fixture) {
    let parent = traj("t1");
    let child = traj("t1-fork");
    closed_wake(
        f,
        &parent,
        &wake("w1"),
        vec![thought(
            &parent,
            &wake("w1"),
            "pin/set",
            pin_set("p", "x", &[]),
        )],
    )
    .await;
    let head = f
        .ledger
        .0
        .head_seq(&parent)
        .await
        .expect("head")
        .expect("some");
    let out = f
        .ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: child.clone(),
            at_seq: head,
            at: at(20),
        })
        .await
        .expect("a prefix ending outside a wake forks");
    assert_eq!(out.edge.child, child);
    assert_eq!(out.edge.parent, parent);
    assert_eq!(out.edge.kind, EdgeKind::Ancestor);
    assert_eq!(out.edge.at_seq, head);
    assert_eq!(
        f.ledger.0.ancestry(&child).await.expect("ancestry"),
        vec![parent]
    );
}

/// Conformance case: `fork_inside_an_open_wake_is_refused_naming_the_wake`.
pub async fn fork_inside_an_open_wake_is_refused_naming_the_wake(f: &Fixture) {
    let parent = traj("t1");
    let w = wake("w-open");
    ok(f, thought(&parent, &w, "wake/start", wake_start())).await;
    let inside = ok(
        f,
        thought(&parent, &w, "step/start", serde_json::json!({ "index": 0 })),
    )
    .await;
    let err = f
        .ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: traj("t1-fork"),
            at_seq: inside.seq,
            at: at(20),
        })
        .await
        .expect_err("a prefix ending inside an open wake is REFUSED, never clipped");
    match err {
        LedgerError::ForkInsideOpenWake {
            parent: p,
            at_seq,
            wake: wk,
            opened_at,
        } => {
            assert_eq!(p, parent);
            assert_eq!(at_seq, inside.seq);
            assert_eq!(wk, w, "the refusal NAMES the wake");
            assert_eq!(opened_at, Seq(1));
        }
        other => panic!("wrong refusal: {other}"),
    }
}

/// Conformance case: `a_refused_fork_writes_nothing`.
pub async fn a_refused_fork_writes_nothing(f: &Fixture) {
    let parent = traj("t1");
    let child = traj("t1-fork");
    let w = wake("w-open");
    ok(f, thought(&parent, &w, "wake/start", wake_start())).await;
    let inside = ok(
        f,
        thought(&parent, &w, "step/start", serde_json::json!({ "index": 0 })),
    )
    .await;
    let _ = f
        .ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: child.clone(),
            at_seq: inside.seq,
            at: at(20),
        })
        .await
        .expect_err("refused");
    // One transaction, or nothing at all: no edge, no end-seed, no child trajectory.
    assert!(f.ledger.0.edges(&child).await.expect("edges").is_empty());
    assert_eq!(f.ledger.0.head_seq(&child).await.expect("head"), None);
    assert!(all_steps(f, &child).await.is_empty());
}

/// Conformance case: `a_fork_never_clips_the_prefix`.
pub async fn a_fork_never_clips_the_prefix(f: &Fixture) {
    let parent = traj("t1");
    let w = wake("w1");
    closed_wake(
        f,
        &parent,
        &w,
        vec![thought(&parent, &w, "pin/set", pin_set("p", "x", &[]))],
    )
    .await;
    let before = all_steps(f, &parent).await;
    let head = f
        .ledger
        .0
        .head_seq(&parent)
        .await
        .expect("head")
        .expect("some");
    f.ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: traj("t1-fork"),
            at_seq: head,
            at: at(20),
        })
        .await
        .expect("fork");
    // The parent is untouched: a fork adds a child, it never truncates history.
    assert_eq!(all_steps(f, &parent).await, before);
    assert_eq!(
        f.ledger.0.head_seq(&parent).await.expect("head"),
        Some(head)
    );
}

/// Conformance case: `the_childs_first_step_is_the_end_seed_marker`.
pub async fn the_childs_first_step_is_the_end_seed_marker(f: &Fixture) {
    let parent = traj("t1");
    let child = traj("t1-fork");
    let w = wake("w1");
    closed_wake(f, &parent, &w, vec![]).await;
    let head = f
        .ledger
        .0
        .head_seq(&parent)
        .await
        .expect("head")
        .expect("some");
    let out = f
        .ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: child.clone(),
            at_seq: head,
            at: at(20),
        })
        .await
        .expect("fork");
    assert_eq!(out.end_seed.seq, Seq(1));
    assert_eq!(out.end_seed.kind.as_str(), "fork/end-seed");
    assert_eq!(out.end_seed.wake, WakeId::seed(&child));
    let steps = all_steps(f, &child).await;
    assert_eq!(
        steps.len(),
        1,
        "the child's live history starts at the end-seed marker"
    );
    assert_eq!(steps[0].id, out.end_seed.id);
}

/// Conformance case: `the_end_seed_carries_the_parent_and_at_seq`.
pub async fn the_end_seed_carries_the_parent_and_at_seq(f: &Fixture) {
    let parent = traj("t1");
    let child = traj("t1-fork");
    let w = wake("w1");
    closed_wake(
        f,
        &parent,
        &w,
        vec![thought(&parent, &w, "pin/set", pin_set("p", "x", &[]))],
    )
    .await;
    let head = f
        .ledger
        .0
        .head_seq(&parent)
        .await
        .expect("head")
        .expect("some");
    let out = f
        .ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: child.clone(),
            at_seq: head,
            at: at(20),
        })
        .await
        .expect("fork");
    // Seed history and live work are never byte-identical, and the marker says where the seam is.
    assert_eq!(
        out.end_seed.body["parent"],
        serde_json::json!(parent.as_str())
    );
    assert_eq!(out.end_seed.body["at_seq"], serde_json::json!(head.0));
}

/// Conformance case: `connected_is_own_chain_plus_ancestry_plus_ref_matches`.
pub async fn connected_is_own_chain_plus_ancestry_plus_ref_matches(f: &Fixture) {
    let parent = traj("t1");
    let child = traj("t1-fork");
    let stranger = traj("t9");
    let w = wake("w1");
    closed_wake(f, &parent, &w, vec![]).await;
    let head = f
        .ledger
        .0
        .head_seq(&parent)
        .await
        .expect("head")
        .expect("some");
    f.ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: child.clone(),
            at_seq: head,
            at: at(20),
        })
        .await
        .expect("fork");
    // A third trajectory nobody forked, reachable only because it mentions the agent's ref.
    ok(
        f,
        evidence(
            &stranger,
            &wake("w9"),
            "mail/delivered",
            mail("s", "x"),
            vec![cite("gh:o/r#42")],
        ),
    )
    .await;
    // And one that matches nothing.
    ok(
        f,
        thought(&traj("t8"), &wake("w8"), "pin/set", pin_set("p", "x", &[])),
    )
    .await;

    let name = AgentName::new("sol");
    f.ledger
        .0
        .put_agent(AgentRow {
            name: name.clone(),
            traj: child.clone(),
            routing_refs: [Ref::new("gh:o/r#42")].into_iter().collect(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("put agent");

    let c = f.ledger.0.connected(&name).await.expect("connected");
    assert_eq!(c.own, child);
    assert_eq!(c.ancestry, vec![parent.clone()]);
    assert_eq!(c.ref_matches, vec![stranger.clone()]);
    let trajs = c.trajectories();
    assert!(trajs.contains(&child) && trajs.contains(&parent) && trajs.contains(&stranger));
    assert!(
        !trajs.contains(&traj("t8")),
        "an unmatched trajectory is not connected"
    );
}

/// Conformance case: `connected_reads_the_agents_row_at_call_time`.
pub async fn connected_reads_the_agents_row_at_call_time(f: &Fixture) {
    let own = traj("t1");
    let other = traj("t2");
    ok(
        f,
        thought(&own, &wake("w1"), "pin/set", pin_set("p", "x", &[])),
    )
    .await;
    ok(
        f,
        evidence(
            &other,
            &wake("w2"),
            "mail/delivered",
            mail("s", "x"),
            vec![cite("linear:ENG-7")],
        ),
    )
    .await;
    let name = AgentName::new("sol");
    let row = AgentRow {
        name: name.clone(),
        traj: own.clone(),
        routing_refs: Default::default(),
        wake_classes: Default::default(),
        model_override: None,
        tick_floor: None,
        digest_rollup: None,
    };
    f.ledger.0.put_agent(row.clone()).await.expect("put agent");
    assert!(f
        .ledger
        .0
        .connected(&name)
        .await
        .expect("connected")
        .ref_matches
        .is_empty());

    f.ledger
        .0
        .put_agent(AgentRow {
            routing_refs: [Ref::new("linear:ENG-7")].into_iter().collect(),
            ..row
        })
        .await
        .expect("relink");
    let c = f.ledger.0.connected(&name).await.expect("connected");
    assert_eq!(
        c.ref_matches,
        vec![other],
        "membership is computed AT NEED, never stamped"
    );
    assert!(c.refs.contains(&Ref::new("linear:ENG-7")));
}

/// Conformance case: `a_late_linked_ref_includes_history_retroactively`.
pub async fn a_late_linked_ref_includes_history_retroactively(f: &Fixture) {
    let own = traj("t1");
    let historic = traj("t-old");
    // A trajectory written long before anybody linked the ref.
    ok(
        f,
        evidence(
            &historic,
            &wake("w0"),
            "mail/delivered",
            mail("ancient", "x"),
            vec![cite("gh:o/r#1")],
        ),
    )
    .await;
    ok(
        f,
        thought(&own, &wake("w1"), "pin/set", pin_set("p", "x", &[])),
    )
    .await;
    let name = AgentName::new("sol");
    f.ledger
        .0
        .put_agent(AgentRow {
            name: name.clone(),
            traj: own.clone(),
            routing_refs: [Ref::new("gh:o/r#1")].into_iter().collect(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("link the ref, after the fact");
    let c = f.ledger.0.connected(&name).await.expect("connected");
    assert!(
        c.trajectories().contains(&historic),
        "linking a ref late includes its history at no cost: inclusion is never written onto rows"
    );
}

/// Conformance case: `linking_a_ref_changes_no_step_row_hash`.
pub async fn linking_a_ref_changes_no_step_row_hash(f: &Fixture) {
    let own = traj("t1");
    let historic = traj("t-old");
    ok(
        f,
        evidence(
            &historic,
            &wake("w0"),
            "mail/delivered",
            mail("ancient", "x"),
            vec![cite("gh:o/r#1")],
        ),
    )
    .await;
    ok(
        f,
        thought(&own, &wake("w1"), "pin/set", pin_set("p", "x", &[])),
    )
    .await;
    let before = f.ledger.0.row_hashes(HashScope::All).await.expect("hashes");

    let name = AgentName::new("sol");
    f.ledger
        .0
        .put_agent(AgentRow {
            name: name.clone(),
            traj: own.clone(),
            routing_refs: [Ref::new("gh:o/r#1")].into_iter().collect(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("link");
    let _ = f.ledger.0.connected(&name).await.expect("connected");
    let after = f.ledger.0.row_hashes(HashScope::All).await.expect("hashes");
    assert_eq!(
        before
            .iter()
            .map(|r| (r.table, r.id.clone(), r.hash.clone()))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|r| (r.table, r.id.clone(), r.hash.clone()))
            .collect::<Vec<_>>(),
        "membership is derived: linking a ref writes nothing onto any entry"
    );
}

/// Conformance case: `connected_writes_nothing`.
pub async fn connected_writes_nothing(f: &Fixture) {
    let own = traj("t1");
    ok(
        f,
        thought(&own, &wake("w1"), "pin/set", pin_set("p", "x", &[])),
    )
    .await;
    let name = AgentName::new("sol");
    f.ledger
        .0
        .put_agent(AgentRow {
            name: name.clone(),
            traj: own.clone(),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("put agent");
    let before = f.ledger.0.row_hashes(HashScope::All).await.expect("hashes");
    let head = f.ledger.0.head_seq(&own).await.expect("head");
    let seen_before = f.tap.seen().len();

    for _ in 0..3 {
        let _ = f.ledger.0.connected(&name).await.expect("connected");
    }
    let after = f.ledger.0.row_hashes(HashScope::All).await.expect("hashes");
    assert_eq!(
        before
            .iter()
            .map(|r| (r.table, r.id.clone(), r.hash.clone()))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|r| (r.table, r.id.clone(), r.hash.clone()))
            .collect::<Vec<_>>(),
        "connected() wrote a row"
    );
    assert_eq!(f.ledger.0.head_seq(&own).await.expect("head"), head);
    assert_eq!(
        f.tap.seen().len(),
        seen_before,
        "connected() broadcast a ledger/step"
    );
}

/// Conformance case: `search_finds_a_step_in_another_trajectory`.
pub async fn search_finds_a_step_in_another_trajectory(f: &Fixture) {
    let a = traj("t1");
    let b = traj("t2");
    ok(
        f,
        thought(
            &a,
            &wake("w1"),
            "pin/set",
            pin_set("here", "the quick brown fox", &[]),
        ),
    )
    .await;
    ok(
        f,
        thought(
            &b,
            &wake("w2"),
            "pin/set",
            pin_set("there", "an elusive pangolin", &[]),
        ),
    )
    .await;
    let hits = f
        .ledger
        .0
        .search(&SearchQuery {
            text: "pangolin".to_string(),
            trajs: vec![],
            limit: 10,
        })
        .await
        .expect("search");
    assert_eq!(hits.len(), 1, "search spans trajectories");
    assert_eq!(hits[0].step.traj, b);
    assert!(!hits[0].snippet.is_empty(), "a hit carries a snippet");
}

/// Conformance case: `a_hit_carries_its_cites`.
pub async fn a_hit_carries_its_cites(f: &Fixture) {
    let t = traj("t1");
    let step = ok(
        f,
        evidence(
            &t,
            &wake("w1"),
            "mail/delivered",
            mail("pangolin sighting", "x"),
            vec![cite("gh:o/r#77")],
        ),
    )
    .await;
    let hits = f
        .ledger
        .0
        .search(&SearchQuery {
            text: "pangolin".to_string(),
            trajs: vec![],
            limit: 10,
        })
        .await
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].step.id, step.id);
    assert_eq!(
        hits[0]
            .step
            .cites
            .iter()
            .map(|c| c.r#ref.clone())
            .collect::<Vec<_>>(),
        vec![Ref::new("gh:o/r#77")],
        "a hit is a whole step: its cites travel with it"
    );
    assert!(hits[0].step.refs.contains(&Ref::new("gh:o/r#77")));
}

/// Conformance case: `search_respects_the_trajectory_filter`.
pub async fn search_respects_the_trajectory_filter(f: &Fixture) {
    let a = traj("t1");
    let b = traj("t2");
    ok(
        f,
        thought(
            &a,
            &wake("w1"),
            "pin/set",
            pin_set("one", "shared pangolin", &[]),
        ),
    )
    .await;
    ok(
        f,
        thought(
            &b,
            &wake("w2"),
            "pin/set",
            pin_set("two", "shared pangolin", &[]),
        ),
    )
    .await;
    let hits = f
        .ledger
        .0
        .search(&SearchQuery {
            text: "pangolin".to_string(),
            trajs: vec![b.clone()],
            limit: 10,
        })
        .await
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].step.traj, b);
}

/// Conformance case: `search_ordering_is_deterministic`.
pub async fn search_ordering_is_deterministic(f: &Fixture) {
    let a = traj("t1");
    let b = traj("t2");
    for i in 0..3 {
        ok(
            f,
            thought(
                &a,
                &wake("w1"),
                "pin/set",
                pin_set(&format!("a{i}"), "pangolin", &[]),
            ),
        )
        .await;
        ok(
            f,
            thought(
                &b,
                &wake("w2"),
                "pin/set",
                pin_set(&format!("b{i}"), "pangolin", &[]),
            ),
        )
        .await;
    }
    let q = SearchQuery {
        text: "pangolin".to_string(),
        trajs: vec![],
        limit: 10,
    };
    let first = f.ledger.0.search(&q).await.expect("search");
    let second = f.ledger.0.search(&q).await.expect("search");
    assert_eq!(first.len(), 6);
    assert_eq!(
        first.iter().map(|h| h.step.id.clone()).collect::<Vec<_>>(),
        second.iter().map(|h| h.step.id.clone()).collect::<Vec<_>>(),
        "the same query twice returns the same order"
    );
    // P1-D19: `seq DESC, traj ASC`, so the two providers can agree without bm25.
    let keys: Vec<(u64, String)> = first
        .iter()
        .map(|h| (h.step.seq.0, h.step.traj.to_string()))
        .collect();
    let mut expected = keys.clone();
    expected.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
    assert_eq!(keys, expected, "search ordering is seq DESC, traj ASC");
    // The limit is honoured, and honoured from the same end.
    let limited = f
        .ledger
        .0
        .search(&SearchQuery { limit: 2, ..q })
        .await
        .expect("search");
    assert_eq!(
        limited
            .iter()
            .map(|h| h.step.id.clone())
            .collect::<Vec<_>>(),
        first[..2]
            .iter()
            .map(|h| h.step.id.clone())
            .collect::<Vec<_>>()
    );
}

/// Conformance case: `a_sealed_rollup_is_readable_by_query`.
pub async fn a_sealed_rollup_is_readable_by_query(f: &Fixture) {
    let t = traj("t1");
    let sealed = f.ledger.0.seal_rollup(seal("r1", &t)).await.expect("seal");
    assert_eq!(sealed.superseded_by, None);
    assert_eq!(
        sealed.notable_refs,
        [Ref::new("gh:o/r#1")].into_iter().collect()
    );

    let rows = f
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![t.clone()],
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(rows, vec![sealed.clone()]);

    // A superseded rollup is excluded by default and included on request.
    f.ledger.0.seal_rollup(seal("r2", &t)).await.expect("seal");
    f.ledger
        .0
        .supersede_rollup(&RollupId::new("r1"), &RollupId::new("r2"))
        .await
        .expect("supersede");
    let live = f
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![t.clone()],
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(
        live.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        vec![RollupId::new("r2")]
    );
    let all = f
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![t.clone()],
            include_superseded: true,
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(all.len(), 2);
}

/// Conformance case: `an_action_intent_then_done_updates_the_journal_row`.
pub async fn an_action_intent_then_done_updates_the_journal_row(f: &Fixture) {
    let id = ActionId::new("a1");
    let row = f
        .ledger
        .0
        .action_intent(NewAction {
            id: Some(id.clone()),
            wake: wake("w1"),
            idem_key: IdemKey::new("k1"),
            kind: "gh:comment".to_string(),
            payload: serde_json::json!({ "body": "hi" }),
            at: at(0),
        })
        .await
        .expect("intent");
    assert_eq!(row.status, ActionStatus::Intent);
    assert_eq!(row.result, None);

    f.ledger
        .0
        .action_done(
            &id,
            ActionStatus::Done,
            serde_json::json!({ "url": "https://x/1" }),
        )
        .await
        .expect("done");
    let rows = f
        .ledger
        .0
        .actions(&ActionQuery {
            ids: vec![id.clone()],
            ..Default::default()
        })
        .await
        .expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, ActionStatus::Done);
    assert_eq!(
        rows[0].result,
        Some(serde_json::json!({ "url": "https://x/1" }))
    );
    assert!(rows[0].done_at.is_some());
    // The journal is not the ledger of record: `actions` is a mutable status row (P1-D11), and
    // filtering by status is how Phase 2 will find the unfinished ones.
    let by_status = f
        .ledger
        .0
        .actions(&ActionQuery {
            status: Some(ActionStatus::Intent),
            ..Default::default()
        })
        .await
        .expect("query");
    assert!(by_status.is_empty());
}

/// Conformance case: `trajectory_view_returns_steps_edges_and_rollups`.
pub async fn trajectory_view_returns_steps_edges_and_rollups(f: &Fixture) {
    let parent = traj("t1");
    let child = traj("t1-fork");
    let w = wake("w1");
    closed_wake(
        f,
        &parent,
        &w,
        vec![thought(&parent, &w, "pin/set", pin_set("p", "x", &[]))],
    )
    .await;
    let head = f
        .ledger
        .0
        .head_seq(&parent)
        .await
        .expect("head")
        .expect("some");
    f.ledger
        .0
        .fork(Fork {
            parent: parent.clone(),
            child: child.clone(),
            at_seq: head,
            at: at(20),
        })
        .await
        .expect("fork");
    f.ledger
        .0
        .seal_rollup(seal("r1", &child))
        .await
        .expect("seal");
    let name = AgentName::new("sol");
    f.ledger
        .0
        .put_agent(AgentRow {
            name: name.clone(),
            traj: child.clone(),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("put agent");

    let view = f.ledger.0.trajectory_view(&child).await.expect("view");
    assert_eq!(view.traj, child);
    assert_eq!(view.steps.len(), 1, "the end-seed marker");
    assert_eq!(view.steps[0].kind.as_str(), "fork/end-seed");
    assert_eq!(view.edges.len(), 1);
    assert_eq!(view.edges[0].parent, parent);
    assert_eq!(
        view.rollups
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
        vec![RollupId::new("r1")]
    );
    assert_eq!(view.agent.map(|a| a.name), Some(name));
}

/// The body of the probe step type the unknown-type cases register and then withdraw.
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
struct ProbeNote {
    text: String,
}

/// The error a whole-trajectory read produced, for the cases that expect a refusal.
async fn all_steps_err(f: &Fixture, t: &TrajId) -> LedgerError {
    f.ledger
        .0
        .steps(&StepQuery {
            trajs: vec![t.clone()],
            ..Default::default()
        })
        .await
        .expect_err("the read must be REFUSED, not silently truncated")
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
