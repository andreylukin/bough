//! §0.2 runtime invariant for `bough-plugin-boundary-instructions`:
//!
//! **Every projection assembled for a live agent carries [`crate::BOUNDARY_BLOCK`] byte for
//! byte, at the tightest budget the assembler will accept.** One source, checked against
//! ASSEMBLED projections rather than asserted in prose: a section that renders but is dropped by
//! a degradation rung would pass any registry check and still leave a model unbounded.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::Ledger;
use bough_plugin_projection::{AssembleRequest, Assembled, Projection};

const NAME: &str = "every_assembled_projection_carries_the_boundary";

/// PURE: the check over one assembled projection.
pub fn check(assembled: &Assembled) -> Result<(), String> {
    if assembled
        .sections
        .iter()
        .any(|s| s.id == crate::section_id() && s.body == crate::BOUNDARY_BLOCK)
    {
        return Ok(());
    }
    Err(format!(
        "the projection assembled for `{}` carries no verbatim write-boundary section (sections: \
         {})",
        assembled.agent,
        assembled
            .sections
            .iter()
            .map(|s| s.id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    // Both are peeked: on the way down either may already be gone, and there is nothing to state
    // about a projection that no longer exists.
    let (Some(projection), Some(ledger)) =
        (ctx.peek_live::<Projection>(), ctx.peek_live::<Ledger>())
    else {
        return Ok(());
    };
    let agents = ledger.0.agents().await.map_err(|e| fail(e.to_string()))?;
    for agent in agents {
        let req = AssembleRequest {
            agent: agent.name.clone(),
            wake: None,
            at: chrono::Utc::now(),
            // The tightest budget there is: every rung of the ladder runs, and a `Never` section
            // is the only kind that can survive it.
            budget: Some(1),
            as_of: None,
        };
        match projection.0.assemble(&req).await {
            // Assembling is not this row's job; a projection that cannot be built at all is the
            // assembler's violation to report, not the boundary's.
            Err(_) => continue,
            Ok(out) => check(&out).map_err(fail)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_projection::{Position, RenderedSection, SectionCites, SectionId, Slot};

    fn assembled(sections: Vec<RenderedSection>) -> Assembled {
        Assembled {
            agent: bough_plugin_ledger::AgentName::new("sol"),
            sections,
            flags: Default::default(),
            tokens: 0,
            budget: 1,
            cites: SectionCites::default(),
        }
    }

    fn section(id: &str, body: &str) -> RenderedSection {
        RenderedSection {
            id: SectionId::new(id),
            position: Position::band(Slot::Identity),
            title: crate::SECTION_TITLE.to_string(),
            body: body.to_string(),
            cites: SectionCites::default(),
            tokens: 0,
            degraded: None,
        }
    }

    #[test]
    fn a_projection_carrying_the_verbatim_block_passes() {
        let out = assembled(vec![section("boundary", crate::BOUNDARY_BLOCK)]);
        assert!(check(&out).is_ok());
    }

    /// A PARAPHRASE is not the boundary: the check is byte equality, so a second wording cannot
    /// pass for the one source.
    #[test]
    fn a_paraphrased_boundary_is_a_violation() {
        let out = assembled(vec![section("boundary", "don't post to slack, ok?")]);
        let err = check(&out).expect_err("a paraphrase is not the block");
        assert!(err.contains("no verbatim write-boundary"), "{err}");
    }

    #[test]
    fn a_projection_with_no_boundary_section_is_a_violation() {
        let out = assembled(vec![section("identity", "you are sol")]);
        assert!(check(&out).is_err());
    }
}
