//! Invariant (P5-D11): the persona section is OWNED by the `leader` row's fiber and SCOPED to the
//! target agent by spec. Registering it through the AGENT's ctx (the `worker-spawn` precedent)
//! would tie it to the agent's lifetime, and then moving the set would depend on the old agent
//! being torn down. Owning it here is exactly what makes the SWAP a config edit.

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_ledger::AgentName;
use bough_plugin_projection::{
    DropPriority, Place, Position, ProjectionError, ProjectionHandle, SectionBody, SectionCites,
    SectionId, SectionRender, SectionRequest, SectionScope, SectionSpec, Slot,
};

/// The section id the leader's persona is contributed under. Deliberately NOT `"persona"`: the
/// leader's persona moves with the leader SET, where a lane's moves with the lane list (P5-D17).
/// Two ids also mean the two sections COMPOSE for the leader rather than shadow each other — the
/// leader is an ordinary lane that additionally leads.
pub const SECTION_ID: &str = "leader.persona";

/// The title the band renders under.
pub const TITLE: &str = "Leading";

/// Identity/After: who the agent is, then what leading means for it.
pub const POSITION: Position = Position {
    slot: Slot::Identity,
    place: Place::After,
};

/// A section whose body is the row's configured text.
struct Persona(String);

#[async_trait::async_trait]
impl SectionRender for Persona {
    async fn render(&self, _req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        // An empty persona contributes NOTHING rather than an empty band (the `about-line`
        // precedent).
        if self.0.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(SectionBody {
            title: TITLE.to_string(),
            body: self.0.clone(),
            cites: SectionCites::default(),
        }))
    }
}

/// The spec, scoped to `target` by SPEC rather than by whose ctx registered it.
pub fn spec(target: &AgentName, text: &str) -> SectionSpec {
    SectionSpec {
        id: SectionId::new(SECTION_ID),
        position: POSITION,
        scope: SectionScope::Agent,
        agent: Some(target.clone()),
        // Identity is never dropped (§5): an answer wake must always know who it is.
        priority: DropPriority::Never,
        render: Arc::new(Persona(text.to_string())),
    }
}

/// Register the persona section for `target`, owned by the CALLING row's ctx.
pub async fn register(
    ctx: &Context,
    projection: &ProjectionHandle,
    target: &AgentName,
    text: &str,
) -> Result<EffectHandle, PluginError> {
    projection.section(ctx, spec(target, text)).await
}
