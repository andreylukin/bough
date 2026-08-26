//! Invariant: assembly is DETERMINISTIC (§5). Seven steps in order — connected, the six bands,
//! the contributed sections, `order()`, the `projection/assemble` waterfall, the degradation
//! ladder, finalize — with the waterfall BETWEEN rendering and degradation so a listener may add a
//! section and still be budgeted. Nothing in the request path reads a clock, the filesystem, or a
//! model; `at` comes from the request.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bough_plugin_projection::{
    invariant as pinv, order, AssembleRequest, Assembled, Draft, DropPriority, Flag,
    ProjectionAssemble, ProjectionError, RenderedSection, SectionCites, SectionId, SectionRequest,
};

use crate::bands;
use crate::degrade::{self, Cut};
use crate::Assembler;

/// The leading in-context line `Assembled::to_text` prints when a projection was degraded.
/// Degradation of pins, digest or mail is NEVER silent (§5) — this line is how the model learns.
pub fn flag_line(flags: &BTreeSet<Flag>) -> String {
    if flags.is_empty() {
        return String::new();
    }
    // ONE spelling: `Assembled::to_text` renders the same line from the same words, so the budget
    // arithmetic here and the text the model reads can never drift apart.
    let names: Vec<&str> = flags.iter().map(Flag::word).collect();
    format!("> DEGRADED: {}\n\n", names.join(", "))
}

/// The seven steps, in order.
pub async fn assemble(a: &Assembler, req: &AssembleRequest) -> Result<Assembled, ProjectionError> {
    // 1. Membership, derived at need (§3). Writes nothing.
    let connected = Arc::new(a.ledger.0.connected(&req.agent).await?);

    let sreq = SectionRequest {
        agent: req.agent.clone(),
        wake: req.wake.clone(),
        at: req.at,
        ledger: a.ledger.clone(),
        connected: Arc::clone(&connected),
    };
    let cfg = &*a.cfg;

    // 2. The six built-in bands, in `Slot` order. A band with no input renders NOTHING — not an
    //    empty header — so a zero-rollup ledger assembles cleanly (Phase 4 produces tiers).
    let mut sections: Vec<RenderedSection> = Vec::new();
    if let Some(s) = bands::identity(&sreq, cfg).await? {
        sections.push(s);
    }
    let mut pins = {
        let trajs: Vec<_> = connected.trajectories().into_iter().collect();
        let mut p = a.ledger.0.live_pins(&trajs).await?;
        bands::sort_pins(&mut p);
        p
    };
    if let Some(s) = bands::pins_section(&pins) {
        sections.push(s);
    } else {
        pins = Vec::new();
    }
    if let Some(s) = bands::digest(&sreq, cfg).await? {
        sections.push(s);
    }
    sections.extend(bands::tiers(&sreq, cfg).await?);
    let (tail_section, tail_steps) = bands::tail(&sreq, cfg).await?;
    if let Some(s) = tail_section {
        sections.push(s);
    }
    let (mail_section, mail_steps) = bands::mail(&sreq, cfg).await?;
    if let Some(s) = mail_section {
        sections.push(s);
    }

    // 3. Every registered section this agent admits; agent scope shadows global by `SectionId`.
    let mut priorities: BTreeMap<SectionId, DropPriority> = BTreeMap::new();
    for spec in a.registry.for_agent(&req.agent) {
        priorities.insert(spec.id.clone(), spec.priority);
        let rendered =
            spec.render
                .render(&sreq)
                .await
                .map_err(|e| ProjectionError::SectionRender {
                    id: spec.id.clone(),
                    detail: e.to_string(),
                })?;
        // `Ok(None)` ⇒ the section does not appear at all.
        if let Some(body) = rendered {
            let text = format!("## {}\n\n{}\n", body.title, body.body);
            sections.push(RenderedSection {
                id: spec.id.clone(),
                position: spec.position,
                title: body.title,
                body: body.body,
                cites: body.cites,
                tokens: bough_plugin_projection::tokens::count(&text),
                degraded: None,
            });
        }
    }

    // 4. The fixed order: `(Slot, Place, SectionId)`, never registration order (P1-D8).
    order::order(&mut sections);

    // 5. The waterfall, BETWEEN rendering and degradation, so a listener's section is budgeted.
    let budget = req.budget.unwrap_or(cfg.budget_tokens);
    let draft = Draft {
        request: Arc::new(req.clone()),
        sections,
        budget,
        flags: BTreeSet::new(),
    };
    let mut draft = a.ctx.waterfall::<ProjectionAssemble>(draft).await;
    // A listener may have appended; the order is the assembler's to hold, not the listener's.
    order::order(&mut draft.sections);

    // 6. Degrade in the fixed reverse order, stopping as soon as it fits.
    let effective = bough_plugin_projection::tokens::effective_budget(draft.budget, cfg.headroom);
    let cut = Cut::new(cfg.clone(), priorities, tail_steps, pins, mail_steps);
    degrade::degrade(&mut draft, &cut, effective);

    // 7. Finalize. `cites` is the union of every SURVIVING section's cites — exactly what the
    //    model-visible ⟺ ledgered invariant reads.
    let mut cites = SectionCites::default();
    for s in &draft.sections {
        cites = cites.union(&s.cites);
        pinv::record(pinv::Obs {
            fiber: a.ctx.fiber_uid(),
            section: s.id.clone(),
            cites: s.cites.clone(),
        });
    }
    Ok(Assembled {
        agent: req.agent.clone(),
        tokens: degrade::draft_tokens(&draft),
        budget: effective,
        sections: draft.sections,
        flags: draft.flags,
        cites,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bough_plugin_projection::{Place, Position, Projector, Slot};

    fn listener_section(words: usize) -> RenderedSection {
        let body = vec!["a listener wrote this line"; words].join(" ");
        let mut s = RenderedSection {
            id: SectionId::new("listener"),
            position: Position {
                slot: Slot::Tail,
                place: Place::After,
            },
            title: "Listener".into(),
            body,
            cites: SectionCites::default(),
            tokens: 0,
            degraded: None,
        };
        bands::remeasure(&mut s);
        s
    }

    #[tokio::test]
    async fn the_waterfall_runs_between_render_and_degrade() {
        let f = Fixture::memory().await;
        f.seed_agent().await;
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<usize>::new()));
        let s2 = seen.clone();
        f.ctx
            .on_waterfall::<ProjectionAssemble, _, _>(move |mut d: Draft, next| {
                let s2 = s2.clone();
                async move {
                    // The bands are already rendered when the waterfall sees the draft…
                    s2.lock().push(d.sections.len());
                    // …and nothing has been degraded yet.
                    assert!(d.flags.is_empty(), "degradation has not run yet");
                    d.sections.push(listener_section(1));
                    next.run(d).await
                }
            })
            .await
            .unwrap();

        let out = f
            .assembler()
            .assemble(&assemble_request("sol"))
            .await
            .unwrap();
        assert_eq!(seen.lock().len(), 1, "the waterfall ran exactly once");
        assert!(seen.lock()[0] >= 1, "it saw the rendered bands");
        assert!(
            out.sections.iter().any(|s| s.id.as_str() == "listener"),
            "the listener's section survived into the result"
        );
    }

    #[tokio::test]
    async fn a_listener_added_section_is_budgeted() {
        let f = Fixture::memory().await;
        f.seed_agent().await;
        let bare = f
            .assembler()
            .assemble(&assemble_request("sol"))
            .await
            .unwrap();

        f.ctx
            .on_waterfall::<ProjectionAssemble, _, _>(|mut d: Draft, next| async move {
                d.sections.push(listener_section(5));
                next.run(d).await
            })
            .await
            .unwrap();
        let fat = f
            .assembler()
            .assemble(&assemble_request("sol"))
            .await
            .unwrap();
        assert!(
            fat.tokens > bare.tokens,
            "the added section is counted: {} vs {}",
            fat.tokens,
            bare.tokens
        );

        // And it is subject to the ladder like any other: a tiny budget drops it.
        let mut req = assemble_request("sol");
        req.budget = Some(20);
        let squeezed = f.assembler().assemble(&req).await.unwrap();
        assert!(
            !squeezed
                .sections
                .iter()
                .any(|s| s.id.as_str() == "listener"),
            "an unbudgeted listener section would have survived the ladder"
        );
    }

    #[tokio::test]
    async fn assembly_reads_no_clock() {
        let f = Fixture::memory().await;
        f.seed_agent().await;
        let a = f
            .assembler()
            .assemble(&assemble_request("sol"))
            .await
            .unwrap();
        // Real time passes between the two calls; `at` does not.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let b = f
            .assembler()
            .assemble(&assemble_request("sol"))
            .await
            .unwrap();
        assert_eq!(
            a.to_text(),
            b.to_text(),
            "the text is a function of (ledger, request, config) alone"
        );
    }
}
