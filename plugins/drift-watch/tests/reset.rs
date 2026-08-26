//! §8's one-command reset, over a REAL store: it rebuilds the digest from raw evidence, appends a
//! fresh about-line whose STATE half cites the raw steps and whose INTENT half is empty, repoints
//! the agent row, and leaves every sealed tier exactly as it was.

mod common;

use bough_plugin_about_line::{AboutLine, ABOUT_LINE};
use bough_plugin_drift_watch::{ResetRequest, DRIFT_RESET};
use bough_plugin_ledger::{HashScope, Ref};
use bough_plugin_rollups::Attribution;

async fn reset(h: &common::Harness) -> bough_plugin_drift_watch::ResetReport {
    h.drift
        .reset(&ResetRequest {
            agent: common::agent(),
            traj: common::traj(),
            at: common::at(),
            attribution: Attribution::System,
        })
        .await
        .expect("the reset runs")
}

async fn about_line(h: &common::Harness) -> (bough_plugin_ledger::Step, AboutLine) {
    let step = common::steps_of_kind(h, ABOUT_LINE)
        .await
        .pop()
        .expect("the reset appended an about-line");
    let line: AboutLine =
        serde_json::from_value((*step.body).clone()).expect("the body is an AboutLine");
    (step, line)
}

#[tokio::test]
async fn reset_rebuilds_the_digest_from_raw_evidence() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;
    // A sealed tier exists, so "read raw, not the summary" is a real choice and not the only one.
    common::seal_tier(&h, 1, 4).await;

    let report = reset(&h).await;

    // The seam was asked for a rebuild FROM RAW: that flag is what makes the provider ignore the
    // standing digest.
    assert_eq!(
        *h.summarizer.from_raw_seen.lock(),
        vec![true],
        "the reset must ask for `from_raw: true`"
    );

    // And what it read was the raw rows, not the tier block.
    let digest = h
        .ledger
        .0
        .rollups(&bough_plugin_ledger::RollupQuery {
            trajs: vec![common::traj()],
            kind: Some(bough_plugin_ledger::RollupKind::Digest),
            include_superseded: true,
            ..Default::default()
        })
        .await
        .expect("the digest reads")
        .pop()
        .expect("a digest was sealed");
    assert_eq!(digest.id, report.digest);
    assert_eq!(digest.body["from_raw"], serde_json::json!(true));
    let evidence = digest.body["evidence"]
        .as_array()
        .expect("evidence is a list");
    assert_eq!(
        evidence.len(),
        8,
        "all eight raw rows were read: {evidence:?}"
    );

    // There was no digest before, so nothing was replaced — and the report says so rather than
    // inventing a predecessor.
    assert_eq!(report.replaced_digest, None);

    // A second reset replaces the first digest, and the first is SUPERSEDED, never edited.
    let second = reset(&h).await;
    assert_eq!(second.replaced_digest, Some(report.digest.clone()));
    let hashes = common::hashes(&h, HashScope::Rollups).await;
    let old = hashes
        .iter()
        .find(|(id, _, _)| *id == report.digest.to_string())
        .expect("the first digest is still in the ledger");
    assert_eq!(
        old.2,
        Some(second.digest.to_string()),
        "the replaced digest carries `superseded_by`, and nothing else changed"
    );
}

#[tokio::test]
async fn reset_appends_an_about_line_whose_state_half_cites_raw_steps() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;
    let raw: Vec<String> = common::all_steps(&h)
        .await
        .iter()
        .map(|s| s.id.to_string())
        .collect();

    let report = reset(&h).await;
    let (step, line) = about_line(&h).await;
    assert_eq!(step.id, report.about_line);

    // EVIDENCE, so the LEDGER itself refused a line with no cites — this is not only an assertion
    // here. And every cite is a raw step of this trajectory.
    assert_eq!(step.class, bough_plugin_ledger::Class::Evidence);
    assert!(!step.cites.is_empty());
    for cite in step.cites.iter() {
        let id = cite
            .r#ref
            .as_str()
            .strip_prefix("step:")
            .unwrap_or_else(|| panic!("a reset cites raw STEPS: {}", cite.r#ref));
        assert!(
            raw.contains(&id.to_string()),
            "cited a step that is not raw evidence: {id}"
        );
    }
    // The state half restates those rows: eight of them, four thoughts, four calls.
    assert!(line.state.contains("rebuilt from raw evidence"), "{line:?}");
    assert!(line.state.contains("4 thoughts"), "{line:?}");
    assert!(line.state.contains("4 tool calls"), "{line:?}");

    // The `drift/reset` step names the same about-line, and cites both it and the new digest.
    let reset_step = common::steps_of_kind(&h, DRIFT_RESET)
        .await
        .pop()
        .expect("the reset appended its own step");
    assert_eq!(reset_step.id, report.reset_step);
    assert_eq!(reset_step.class, bough_plugin_ledger::Class::Evidence);
    assert_eq!(
        reset_step.body["about_line"],
        serde_json::json!(step.id.to_string())
    );
    assert!(
        reset_step.refs.contains(&Ref::step(&step.id)),
        "{:?}",
        reset_step.refs
    );
    assert!(
        reset_step.refs.contains(&Ref::rollup(&report.digest)),
        "{:?}",
        reset_step.refs
    );
    // Attribution is recorded: Phase 5's leader writes `agent` here with no shape change.
    assert_eq!(
        reset_step.body["attribution"]["by"],
        serde_json::json!("system")
    );
}

#[tokio::test]
async fn reset_leaves_the_intent_half_empty() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;

    // The agent HAD an intent, and a loud one. A reset that carried it forward would be exactly
    // the drift the reset exists to undo.
    common::append(
        &h,
        ABOUT_LINE,
        serde_json::json!({
            "state": "was doing the old thing",
            "intent": "keep doing the old thing forever",
            "of_wake": "w1",
        }),
        vec![bough_plugin_ledger::Cite {
            r#ref: Ref::new("step:seed"),
            url: None,
        }],
    )
    .await;

    reset(&h).await;
    let (_, line) = about_line(&h).await;
    assert_eq!(line.intent, "", "the intent half starts EMPTY (§8)");
    assert!(
        !line.state.is_empty(),
        "the state half is rebuilt, not blanked"
    );
    assert!(
        !line.state.contains("old thing"),
        "the state half is rebuilt from raw evidence, not copied from the previous line: {line:?}"
    );

    // And the row's own invariant agrees, over what actually happened.
    bough_plugin_drift_watch::invariant::evaluate(&bough_plugin_drift_watch::invariant::seen())
        .expect("a real reset satisfies `a_reset_rebuilds_and_never_reseals`");
}

#[tokio::test]
async fn reset_leaves_every_sealed_tier_untouched() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;
    let a = common::seal_tier(&h, 1, 4).await;
    let b = common::seal_tier(&h, 5, 8).await;

    let before: Vec<_> = common::tiers(&h).await;
    let before_hashes = common::hashes(&h, HashScope::Rollups).await;

    let report = reset(&h).await;

    // Counted on both sides, and reported — the claim is checkable by the reader.
    assert_eq!(report.tiers_before, 2);
    assert_eq!(report.tiers_after, 2);

    let after = common::tiers(&h).await;
    assert_eq!(before, after, "a sealed tier row changed across the reset");
    assert_eq!(
        after.iter().map(|r| &r.id).collect::<Vec<_>>(),
        vec![&a.id, &b.id]
    );
    assert!(
        after.iter().all(|r| r.superseded_by.is_none()),
        "no tier was superseded"
    );

    // The only rollup hashes that moved are the digest's — the tiers' are byte-identical.
    let after_hashes = common::hashes(&h, HashScope::Rollups).await;
    for (id, hash, superseded) in &before_hashes {
        let now = after_hashes
            .iter()
            .find(|(i, _, _)| i == id)
            .unwrap_or_else(|| panic!("sealed row `{id}` vanished"));
        assert_eq!(
            (hash, superseded),
            (&now.1, &now.2),
            "sealed row `{id}` changed"
        );
    }

    // No `rollup/sealed` of kind `tier` was appended either: the reset seals a DIGEST and nothing
    // else.
    let sealed = common::steps_of_kind(&h, "rollup/sealed").await;
    assert!(
        sealed
            .iter()
            .all(|s| s.body.get("kind") != Some(&serde_json::json!("tier"))),
        "the reset appended a tier seal: {sealed:?}"
    );
}

#[tokio::test]
async fn reset_repoints_the_agent_row_at_the_new_digest() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;
    assert_eq!(
        h.ledger
            .0
            .agent(&common::agent())
            .await
            .unwrap()
            .unwrap()
            .digest_rollup,
        None
    );

    let report = reset(&h).await;

    let row = h
        .ledger
        .0
        .agent(&common::agent())
        .await
        .expect("the agent row reads")
        .expect("the agent has a row");
    assert_eq!(
        row.digest_rollup,
        Some(report.digest.clone()),
        "the identity band must render the digest the reset just built, not the one it replaced"
    );

    // A second reset moves the pointer again, and nothing else about the row moves.
    let second = reset(&h).await;
    let row2 = h.ledger.0.agent(&common::agent()).await.unwrap().unwrap();
    assert_eq!(row2.digest_rollup, Some(second.digest));
    assert_eq!(row2.traj, row.traj);
    assert_eq!(row2.routing_refs, row.routing_refs);
}

#[tokio::test]
async fn a_trajectory_with_no_raw_evidence_is_refused_rather_than_invented() {
    let h = common::harness().await;
    // An agent row, a trajectory, and not one raw step on it.
    h.ledger
        .0
        .put_agent(bough_plugin_ledger::AgentRow {
            name: common::agent(),
            traj: common::traj(),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row writes");

    let err = h
        .drift
        .reset(&ResetRequest {
            agent: common::agent(),
            traj: common::traj(),
            at: common::at(),
            attribution: Attribution::System,
        })
        .await
        .expect_err("a rebuild `from raw evidence` with no raw evidence must refuse");
    assert!(err.to_string().contains("no raw evidence"), "{err}");
    // And it refused BEFORE writing anything.
    assert!(common::all_steps(&h).await.is_empty());
    assert!(h.summarizer.from_raw_seen.lock().is_empty());
}
