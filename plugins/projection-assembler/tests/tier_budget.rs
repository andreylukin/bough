//! Invariant (§5): with REAL sealed tiers in the ledger, the degradation ladder drops fine tiers
//! first, shrinks the verbatim tail to its floor next, and only then takes a coarse tier — and
//! pins, digest and mail are the last to go and never go silently.
//!
//! Phase 1 built the ladder against ZERO rollups, so rungs 1 and 3 were unreachable. These are the
//! same rules asserted end to end, over blocks sealed with the `bough-plugin-rollups` vocabulary.
//!
//! Every claim here is a SWEEP rather than one hand-picked budget: the ladder's order is "X is
//! given up at a tighter budget than Y", and a sweep states that directly instead of encoding a
//! token count that any wording change would move.

mod support;

use bough_plugin_projection::Flag;
use bough_plugin_rollups::Beneath;
use support::*;

/// What survived one assembly at one budget.
#[derive(Debug)]
struct Shape {
    tiers: Vec<u8>,
    tail_steps: usize,
    pins_collapsed: bool,
    mail_collapsed: bool,
    digest_truncated: bool,
    has_pins: bool,
    has_digest: bool,
    has_mail: bool,
}

async fn probe(h: &Harness, budget: usize) -> Shape {
    let a = h.assemble(cfg(budget)).await;
    let tiers: Vec<u8> = a
        .sections
        .iter()
        .filter_map(|s| bough_plugin_projection_assembler::bands::tier_of(&s.id))
        .collect();
    let tail_steps = body(&a, "tail")
        .map(|b| b.lines().filter(|l| l.starts_with("- #")).count())
        .unwrap_or(0);
    Shape {
        tiers,
        tail_steps,
        pins_collapsed: a.flags.contains(&Flag::PinsDegraded),
        mail_collapsed: a.flags.contains(&Flag::MailDegraded),
        digest_truncated: a.flags.contains(&Flag::DigestDegraded),
        has_pins: ids(&a).iter().any(|i| i == "pins"),
        has_digest: ids(&a).iter().any(|i| i == "digest"),
        has_mail: ids(&a).iter().any(|i| i == "mail"),
    }
}

/// A trajectory with all six bands populated and three sealed tiers over it.
async fn seeded() -> Harness {
    let h = Harness::open(Which::Memory);
    h.put_agent(Some("r-digest")).await;
    h.digest(
        "r-digest",
        1,
        30,
        "sol keeps the tree green.\n\nA second paragraph, so truncation has something to take.",
    )
    .await;
    h.pin(
        "p1",
        "gates before commit",
        "`make gates` must be green before every commit",
    )
    .await;
    for n in 1..=20 {
        h.note(
            &format!("s{n}"),
            if n % 2 == 0 { "w2" } else { "w1" },
            "a verbatim step, said at enough length to weigh something in the budget",
        )
        .await;
    }
    h.mail("m1", "ordinary", "andrey", "look at WP-5, at some length")
        .await;
    h.mail(
        "m2",
        "wake",
        "andrey",
        "and at this too, also at some length",
    )
    .await;
    h.tier(
        "r-t1",
        1,
        1,
        10,
        "the fine tier over 1..10, in a sentence long enough to matter",
        &[],
        Beneath::Raw {
            steps: (1..=10)
                .map(|n| bough_plugin_ledger::StepId::new(format!("s{n}")))
                .collect(),
        },
    )
    .await;
    h.tier(
        "r-t2",
        2,
        1,
        20,
        "the middle tier over 1..20, in a sentence long enough to matter",
        &[],
        Beneath::Blocks {
            rollups: vec![bough_plugin_ledger::RollupId::new("r-t1")],
        },
    )
    .await;
    h.tier(
        "r-t3",
        3,
        1,
        20,
        "the coarse tier over 1..20, in a sentence long enough to matter",
        &[],
        Beneath::Blocks {
            rollups: vec![bough_plugin_ledger::RollupId::new("r-t2")],
        },
    )
    .await;
    h
}

/// The tightest budget at which `pred` still holds, walking down. `None` if it never holds.
async fn last_budget_where(h: &Harness, pred: fn(&Shape) -> bool) -> Option<usize> {
    let mut found = None;
    for budget in (20..=1400).step_by(10) {
        if pred(&probe(h, budget).await) {
            found = Some(budget);
        }
    }
    found
}

#[tokio::test]
async fn coarse_survives_and_fine_is_dropped_first() {
    let h = seeded().await;
    let roomy = probe(&h, 100_000).await;
    assert_eq!(
        roomy.tiers,
        vec![3, 2, 1],
        "coarse to fine, all three sealed"
    );

    // Walking down, tier 1 is the first to go and tier 3 the last: for every budget at which any
    // tier survives, the COARSEST is among the survivors.
    let mut saw_partial = false;
    for budget in (20..=1400).step_by(10) {
        let s = probe(&h, budget).await;
        if s.tiers.is_empty() {
            continue;
        }
        assert!(
            s.tiers.contains(&3),
            "budget {budget}: a fine tier outlived the coarse one: {:?}",
            s.tiers
        );
        assert!(
            s.tiers.windows(2).all(|w| w[0] > w[1]),
            "budget {budget}: tiers are not coarse to fine: {:?}",
            s.tiers
        );
        if s.tiers.len() < 3 {
            saw_partial = true;
            assert!(
                !s.tiers.contains(&1) || s.tiers.contains(&2),
                "budget {budget}: tier 1 survived while tier 2 was dropped: {:?}",
                s.tiers
            );
        }
    }
    assert!(saw_partial, "the sweep never actually dropped a tier");
}

#[tokio::test]
async fn the_verbatim_tail_shrinks_to_its_floor_before_a_coarse_tier_goes() {
    let h = seeded().await;
    let floor = cfg(0).tail_floor_steps;

    // Tightening the budget is walking DOWN, so the rung that fires EARLIER is the one whose
    // threshold budget is HIGHER: the loosest budget at which it has already fired.
    let tail_at_floor = last_budget_where(&h, |s| s.tail_steps > 0 && s.tail_steps <= 3)
        .await
        .expect("the tail reaches its floor somewhere in the sweep");
    let coarse_gone = last_budget_where(&h, |s| !s.tiers.contains(&3))
        .await
        .expect("the coarse tier goes somewhere in the sweep");
    assert!(
        coarse_gone < tail_at_floor,
        "the coarse tier went at budget {coarse_gone} while the tail only reached its floor at \
         {tail_at_floor}: rung 3 must come after rung 2"
    );

    // And the floor is a floor: the tail never renders fewer than `tail_floor_steps` while it
    // renders at all.
    for budget in (20..=1400).step_by(10) {
        let s = probe(&h, budget).await;
        assert!(
            s.tail_steps == 0 || s.tail_steps >= floor,
            "budget {budget}: the tail was cut to {} steps, below the floor of {floor}",
            s.tail_steps
        );
    }
}

#[tokio::test]
async fn pins_digest_and_mail_degrade_last_and_never_silently() {
    let h = seeded().await;

    let tiers_gone = last_budget_where(&h, |s| !s.tiers.is_empty())
        .await
        .expect("tiers survive at a roomy budget");
    let pins_go = last_budget_where(&h, |s| !s.pins_collapsed)
        .await
        .expect("pins are verbatim at a roomy budget");
    assert!(
        pins_go <= tiers_gone,
        "pins collapsed at {pins_go} while a tier still stood at {tiers_gone}"
    );

    // The three bands §5 protects are never DROPPED — they collapse, and each collapse raises its
    // own in-context flag.
    let squeezed = probe(&h, 20).await;
    assert!(squeezed.has_pins && squeezed.has_digest && squeezed.has_mail);
    assert!(squeezed.pins_collapsed, "pins collapsed without saying so");
    assert!(squeezed.mail_collapsed, "mail collapsed without saying so");
    assert!(
        squeezed.digest_truncated,
        "the digest was truncated without saying so"
    );

    let text = h.assemble(cfg(20)).await.to_text();
    assert!(
        text.contains("> DEGRADED:"),
        "the model is told, in context: {text}"
    );
}

/// Phase 1's pure `bands::tests::a_tier_whose_notable_refs_miss_the_agent_is_filtered_out`, end to
/// end over sealed rows: the block never reaches the DRAFT at all, so no rung can be blamed for it.
#[tokio::test]
async fn a_tier_whose_notable_refs_miss_the_agent_never_reaches_the_draft() {
    let h = Harness::open(Which::Memory);
    h.put_agent(None).await;
    h.note("s1", "w1", "one").await;
    h.tier(
        "r-hit",
        1,
        1,
        1,
        "about the agent's own work",
        &[MINE],
        Beneath::Raw { steps: vec![] },
    )
    .await;
    h.tier(
        "r-miss",
        1,
        1,
        1,
        "about somebody else entirely",
        &["gh:other/repo#99"],
        Beneath::Raw { steps: vec![] },
    )
    .await;
    h.tier(
        "r-everyone",
        1,
        1,
        1,
        "notable to everyone",
        &[],
        Beneath::Raw { steps: vec![] },
    )
    .await;

    // A budget with room to spare: nothing here is a degradation decision.
    let a = h.assemble(cfg(100_000)).await;
    let tier = body(&a, "tier-254").expect("one tier band");
    assert!(tier.contains("about the agent's own work"), "{tier}");
    assert!(
        tier.contains("notable to everyone"),
        "empty notable_refs means notable to everyone (P1-D13): {tier}"
    );
    assert!(!tier.contains("somebody else entirely"), "{tier}");
    assert!(
        !a.cites.rollups.iter().any(|r| r.as_str() == "r-miss"),
        "a filtered block must not be cited either"
    );
}
