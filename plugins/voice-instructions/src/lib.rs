//! §10: HOW an agent answers — the standing voice block, in ONE crate.
//!
//! It sits beside the write boundary and is shaped the same way: one global section at the
//! identity band, `DropPriority::Never`, no configuration. The boundary bounds what may be DONE;
//! this bounds how much is SAID. Both are the harness's, not a deployment's.
//!
//! Why it exists (Andrey, 2026-09-01): the answers were essays. The chat pane is narrow, the
//! transcript already shows every step, and length is a cost he pays rather than effort the model
//! shows. The wording follows the published harness prompts that solved this (Claude Code's tone
//! and output-efficiency rules) with bough's own facts in place of theirs.

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_projection::{
    DropPriority, Place, Position, Projection, SectionBody, SectionCites, SectionId, SectionRender,
    SectionRequest, SectionScope, SectionSpec,
};

pub const PLUGIN_NAME: &str = "voice-instructions";

/// The standing voice block. ONE source for every path that shows it to a model.
pub const VOICE_BLOCK: &str = "\
Voice — how you answer, every turn.

Be terse. One to three lines is the normal answer and a single word is often the whole of it. \
Andrey reads you in a narrow pane between other things: length is a cost he pays, never effort you \
show him.

Lead with the answer. The first sentence is what happened, what you found, or what you changed. \
No preamble — not `Let me`, not `I'll now`, not restating what was asked — and no closing summary \
of the turn he just watched. Act, then stop.

Report the OUTCOME, not the walk. The transcript already holds every step you took; what it does \
not hold is what they meant. Say plainly what you did NOT do, or could not verify, in the same \
breath — an unverified claim costs him more than a long answer ever would.

Answer the question that was asked, and only it. No alternatives he did not ask about, no hedging \
without information, no explaining code that is on his screen. `X imports Y` beats `it looks like \
X might import Y`; specificity comes from the content, not from more words around it.

Prose, not scaffolding. Headers, bullets and tables are for material that genuinely is a list or a \
table — never for two facts and never to look thorough.

Detail is a request, not a default: when he asks for depth, a design, a review, or a plan, give it \
in full. This block forbids PADDING, never substance. A question you cannot answer briefly and \
honestly is answered at whatever length honesty takes.
";

/// The section id, so a test can find it in an assembled projection by name.
pub fn section_id() -> SectionId {
    SectionId::new("voice")
}

/// The block, for anything that prepends rather than projects.
pub fn block() -> &'static str {
    VOICE_BLOCK
}

/// The section's title. The BODY is [`VOICE_BLOCK`] and nothing else, byte for byte.
pub const SECTION_TITLE: &str = "Voice";

struct VoiceSection;

#[async_trait::async_trait]
impl SectionRender for VoiceSection {
    async fn render(
        &self,
        _req: &SectionRequest,
    ) -> Result<Option<SectionBody>, bough_plugin_projection::ProjectionError> {
        Ok(Some(SectionBody {
            title: SECTION_TITLE.to_string(),
            body: VOICE_BLOCK.to_string(),
            cites: SectionCites::default(),
        }))
    }
}

/// The one spec this row contributes. `Global`, so it reaches residents AND workers; `Never`, so
/// no degradation rung drops it — an agent under pressure is exactly the one that starts padding.
pub fn section_spec() -> SectionSpec {
    SectionSpec {
        id: section_id(),
        position: Position {
            slot: bough_plugin_projection::Slot::Identity,
            place: Place::After,
        },
        scope: SectionScope::Global,
        agent: None,
        priority: DropPriority::Never,
        render: Arc::new(VoiceSection),
    }
}

/// No configuration: how the harness speaks is not a deployment's to vary (§0.2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceConfig {}

/// The row.
pub struct VoicePlugin;

#[async_trait::async_trait]
impl Plugin for VoicePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = VoiceConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["projection"])
    }

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(entry, e))?;
        // A REGISTRATION IS AN EFFECT: unloading this row removes the section and leaves the
        // registry as if the voice had never mounted.
        projection.section(&ctx, section_spec()).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

bough_kernel::register_plugin!(VoicePlugin);

#[cfg(test)]
mod tests {
    use super::*;

    /// The block says the two things that must not drift apart: be brief, and never at the cost
    /// of substance. A voice block that only says "be brief" is how a harness starts truncating
    /// the reviews and designs Andrey actually asked for.
    #[test]
    fn the_block_bounds_padding_and_protects_substance() {
        assert!(VOICE_BLOCK.contains("One to three lines"));
        assert!(VOICE_BLOCK.contains("Lead with the answer"));
        assert!(
            VOICE_BLOCK.contains("Detail is a request"),
            "brevity must not read as permission to leave the thinking out"
        );
        assert!(VOICE_BLOCK.contains("forbids PADDING, never substance"));
    }

    /// It rides the identity band for every agent and is never dropped under pressure.
    #[test]
    fn the_section_is_global_and_undroppable() {
        let spec = section_spec();
        assert_eq!(spec.id, section_id());
        assert_eq!(spec.priority, DropPriority::Never);
        assert!(matches!(spec.scope, SectionScope::Global));
        assert_eq!(spec.position.slot, bough_plugin_projection::Slot::Identity);
    }
}
