//! Invariant (§3, §5): a pin rides EVERY projection verbatim. Sealing a tier over the range a pin
//! sits in does not fold the pin into the summary, and the degradation ladder reaches pins only at
//! rung 4 — after every tier and the whole shrinkable tail have already gone.
//!
//! Phase 1 could assert the second half only over an empty tiers band. With real sealed tiers the
//! claim finally has something to be true against.

mod support;

use bough_plugin_projection::Flag;
use bough_plugin_rollups::Beneath;
use support::*;

#[tokio::test]
async fn a_pin_covered_by_a_sealed_tier_still_rides_the_projection_verbatim() {
    let h = Harness::open(Which::Memory);
    h.put_agent(None).await;
    let pin = h
        .pin(
            "p1",
            "gates before commit",
            "`make gates` must be green before every commit",
        )
        .await;
    for n in 1..=30 {
        h.note(&format!("s{n}"), "w1", "a step, long past the tail window")
            .await;
    }
    // A tier over 1..20 — the range the pin's own step sits in.
    h.tier(
        "r-t1",
        1,
        1,
        20,
        "sol agreed a rule about gates and then worked for a while",
        &[],
        Beneath::Raw {
            steps: vec![bough_plugin_ledger::StepId::new("p1")],
        },
    )
    .await;

    let a = h.assemble(cfg(100_000)).await;
    let tail = body(&a, "tail").expect("a tail");
    assert!(
        !tail.contains("step:p1") && !tail.contains("#1 "),
        "the pin's own row is outside the tail window, which is what makes this a test"
    );
    let pins = body(&a, "pins").expect("the pins band");
    assert!(
        pins.contains("`make gates` must be green before every commit"),
        "a sealed tier over the range summarised the pin away:\n{pins}"
    );
    assert!(
        a.cites.steps.contains(&pin),
        "the projection cites the pin's raw step, not the block that covers it"
    );
}

#[tokio::test]
async fn a_pin_is_never_a_degradation_rungs_first_casualty() {
    let h = Harness::open(Which::Memory);
    h.put_agent(None).await;
    h.pin(
        "p1",
        "the standing rule",
        "stated at enough length that collapsing it actually saves tokens",
    )
    .await;
    for n in 1..=20 {
        h.note(&format!("s{n}"), "w1", "a verbatim step of some length")
            .await;
    }
    for (id, tier, to) in [("r-t1", 1u8, 10u64), ("r-t2", 2, 20)] {
        h.tier(
            id,
            tier,
            1,
            to,
            "a sealed block, said at enough length to weigh something",
            &[],
            Beneath::Raw { steps: Vec::new() },
        )
        .await;
    }

    // Walk the budget down and watch the order the ladder gives things up in.
    let mut pins_verbatim_until = None;
    let mut tiers_stood_until = None;
    let mut tail_full_until = None;
    for budget in (20..=1200).step_by(10) {
        let a = h.assemble(cfg(budget)).await;
        let tiers = a
            .sections
            .iter()
            .filter(|s| bough_plugin_projection_assembler::bands::tier_of(&s.id).is_some())
            .count();
        let tail = body(&a, "tail")
            .map(|b| b.lines().filter(|l| l.starts_with("- #")).count())
            .unwrap_or(0);
        if !a.flags.contains(&Flag::PinsDegraded) {
            pins_verbatim_until = Some(budget.min(pins_verbatim_until.unwrap_or(usize::MAX)));
        }
        if tiers > 0 {
            tiers_stood_until = Some(budget.min(tiers_stood_until.unwrap_or(usize::MAX)));
        }
        if tail > cfg(0).tail_floor_steps {
            tail_full_until = Some(budget.min(tail_full_until.unwrap_or(usize::MAX)));
        }
        // Whatever the budget, the pin is never DROPPED — §5 protects the band itself.
        assert!(
            ids(&a).iter().any(|i| i == "pins"),
            "budget {budget}: the pins band was dropped outright"
        );
    }
    let pins = pins_verbatim_until.expect("pins are verbatim at a roomy budget");
    let tiers = tiers_stood_until.expect("tiers stand at a roomy budget");
    let tail = tail_full_until.expect("the tail is above its floor at a roomy budget");
    assert!(
        pins <= tiers && pins <= tail,
        "pins collapsed at budget {pins}, before the tiers ({tiers}) and the tail ({tail}) were \
         spent: the ladder reached rung 4 too early"
    );

    // And when it finally happens it is never silent.
    let squeezed = h.assemble(cfg(20)).await;
    assert!(squeezed.flags.contains(&Flag::PinsDegraded));
    assert!(squeezed.to_text().contains("> DEGRADED:"));
}
