//! Invariant (§5): the section is a pure read of the newest `about/line` on the agent's own
//! chain — no clock, no LLM, no in-memory copy of the line. A trajectory with no line yet
//! contributes NOTHING rather than an empty band.

use bough_plugin_projection::{
    Place, Position, ProjectionError, SectionBody, SectionCites, SectionId, SectionRender,
    SectionRequest, Slot,
};

use crate::AboutLine;

/// §2: the about-line rides the identity band, right AFTER it — the first thing read after who
/// the agent is, and never a replacement for it.
pub const POSITION: Position = Position {
    slot: Slot::Identity,
    place: Place::After,
};

/// The section's id, and the tie-break key of the section order (P1-D8).
pub fn section_id() -> SectionId {
    SectionId::new("about-line")
}

/// The Identity/After section.
pub struct AboutSection;

#[async_trait::async_trait]
impl SectionRender for AboutSection {
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        let step = crate::newest(&req.ledger, &req.connected.own)
            .await
            .map_err(ProjectionError::from)?;
        let Some(step) = step else { return Ok(None) };
        let Ok(line) = serde_json::from_value::<AboutLine>((*step.body).clone()) else {
            return Ok(None);
        };
        Ok(Some(SectionBody {
            title: "About".to_string(),
            body: crate::render(&line),
            // Model-visible ⟺ ledgered: the section names the row it rendered from.
            cites: SectionCites {
                steps: vec![step.id.clone()],
                rollups: vec![],
            },
        }))
    }
}
