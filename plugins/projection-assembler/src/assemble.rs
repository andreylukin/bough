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
    //
    // `agents` is MUTABLE config a merge may delete (§3), and "an answer wake must always be
    // buildable" (§5): an agent whose row is gone gets the rowless membership and an
    // identity-only projection, never a refusal.
    let connected = Arc::new(match a.ledger.0.connected(&req.agent).await {
        Ok(c) => c,
        Err(bough_plugin_ledger::LedgerError::NoSuchAgent(_)) => {
            bough_plugin_ledger::Connected::rowless()
        }
        Err(e) => return Err(e.into()),
    });

    let sreq = SectionRequest {
        agent: req.agent.clone(),
        wake: req.wake.clone(),
        at: req.at,
        ledger: a.ledger.clone(),
        connected: Arc::clone(&connected),
        as_of: req.as_of,
    };
    let cfg = &*a.cfg;
    // Every request-time default, resolved once and explicitly (§0.2).
    let spec = crate::resolve::resolve_assemble(req, cfg);

    // 2. The six built-in bands, in `Slot` order. A band with no input renders NOTHING — not an
    //    empty header — so a zero-rollup ledger assembles cleanly (Phase 4 produces tiers).
    let mut sections: Vec<RenderedSection> = Vec::new();
    if let Some(s) = bands::identity(&sreq, cfg).await? {
        sections.push(s);
    }
    let mut pins = {
        let trajs: Vec<_> = connected.trajectories().into_iter().collect();
        let mut p = a.ledger.0.live_pins(&trajs).await?;
        // §2.7 item 3: a pin set after `as_of` was not in the projection being reproduced.
        p.retain(|pin| sreq.visible(pin.seq));
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
    let budget = spec.budget;
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
    let cut = Cut::new(
        cfg.clone(),
        priorities,
        spec.default_priority,
        tail_steps,
        pins,
        mail_steps,
    );
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

    /// §5: "an answer wake must always be buildable, leader or no leader". `agents` is mutable
    /// config a merge may delete (§3), so an agent with no row degrades to identity-only rather
    /// than refusing the whole projection.
    #[tokio::test]
    async fn an_agent_with_no_row_still_gets_a_projection() {
        let f = Fixture::memory().await;
        // No `seed_agent()`: the row does not exist.
        assert!(matches!(
            f.ledger
                .0
                .connected(&bough_plugin_ledger::AgentName::new("ghost"))
                .await,
            Err(bough_plugin_ledger::LedgerError::NoSuchAgent(_))
        ));
        let out = f
            .assembler()
            .assemble(&assemble_request("ghost"))
            .await
            .expect("an answer wake must always be buildable");
        assert_eq!(
            out.sections
                .iter()
                .map(|s| s.id.to_string())
                .collect::<Vec<_>>(),
            vec!["identity".to_string()],
            "identity is never dropped, and nothing else has a trajectory to read"
        );
        assert!(out.to_text().contains("trajectory: -"));
    }

    /// A contributed `Place::Before` section sorts ahead of the built-in band whatever its id —
    /// which is only true because the band carries `Place::Band` and not `Place::Before`.
    #[tokio::test]
    async fn a_contributed_before_section_precedes_its_band() {
        for id in ["about", "zeta"] {
            let f = Fixture::memory().await;
            f.seed_agent().await;
            let _tok = f
                .assembler()
                .section(bough_plugin_projection::SectionSpec {
                    id: SectionId::new(id),
                    position: Position {
                        slot: Slot::Identity,
                        place: Place::Before,
                    },
                    scope: bough_plugin_projection::SectionScope::Global,
                    agent: None,
                    priority: bough_plugin_projection::DropPriority::Never,
                    render: std::sync::Arc::new(Fixed),
                })
                .expect("a contributed section registers");
            let out = f
                .assembler()
                .assemble(&assemble_request("sol"))
                .await
                .unwrap();
            let ids: Vec<String> = out.sections.iter().map(|s| s.id.to_string()).collect();
            let (a, b) = (
                ids.iter().position(|x| x == id).expect("contributed"),
                ids.iter().position(|x| x == "identity").expect("band"),
            );
            assert!(a < b, "`{id}` declared Before but sorted {ids:?}");
        }
    }

    /// The six built-in band ids are reserved: a contributed section carrying one would be
    /// undroppable and would shadow the real band in every rung's `index_of`.
    #[tokio::test]
    async fn a_contributed_section_cannot_claim_a_built_in_band_id() {
        let f = Fixture::memory().await;
        for id in ["identity", "pins", "digest", "tail", "mail", "tier-1"] {
            let outcome = f.assembler().section(bough_plugin_projection::SectionSpec {
                id: SectionId::new(id),
                position: Position {
                    slot: Slot::Tail,
                    place: Place::After,
                },
                scope: bough_plugin_projection::SectionScope::Global,
                agent: None,
                priority: bough_plugin_projection::DropPriority::Coarse,
                render: std::sync::Arc::new(Fixed),
            });
            match outcome {
                Err(ProjectionError::ReservedSection { .. }) => {}
                Err(other) => panic!("`{id}`: wrong refusal: {other}"),
                Ok(_) => panic!("`{id}` is a built-in band id and must be refused"),
            }
        }
    }

    struct Fixed;

    #[async_trait::async_trait]
    impl bough_plugin_projection::SectionRender for Fixed {
        async fn render(
            &self,
            _req: &bough_plugin_projection::SectionRequest,
        ) -> Result<Option<bough_plugin_projection::SectionBody>, ProjectionError> {
            Ok(Some(bough_plugin_projection::SectionBody {
                title: "Contributed".into(),
                body: "contributed".into(),
                cites: SectionCites::default(),
            }))
        }
    }
}
