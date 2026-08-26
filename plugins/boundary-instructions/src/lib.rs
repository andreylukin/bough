//! Invariant: there is exactly ONE source of the standing write-boundary text in this tree, and it
//! is [`BOUNDARY_BLOCK`]. Every path that shows the boundary to a model reads this const: the
//! projection section registered here (global, so it reaches residents AND workers), and the block
//! the worker spawner prepends.
//!
//! It is a `const`, NOT config: §7 calls the boundary a security invariant and §0.2 keeps those in
//! code. A patch can disable the ROW — that is Andrey's act — and cannot edit this text.
//!
//! P6-D3: `worker-spawn`'s `WRITE_BOUNDARY` is a second, worker-framed statement of the same four
//! refusals until the merge folds it onto this const. The test at the bottom of this file is what
//! stops the two drifting apart before then.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_projection::{
    DropPriority, Place, Position, Projection, SectionBody, SectionCites, SectionId, SectionRender,
    SectionRequest, SectionScope, SectionSpec,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "boundary-instructions";

/// The standing write-boundary block. ONE source for every path that shows it to a model.
pub const BOUNDARY_BLOCK: &str = "\
Write boundary — this is not advice, it is the limit of what you may do.

Four outward acts are sanctioned, and they go through the harness primitives, never through a raw
tool: open a pull request; push to a pull request that Andrey authored and that is open; reply to,
resolve or close a BOT review thread; change a Linear ticket's status or comment on it.

Everything else that is visible to the team is NOT yours to do. You never send a message as Andrey
— not in Slack, not anywhere — and you never create a ticket. When the work calls for one of those,
write a DRAFT with `draft_message` or `draft_ticket` and say you did; Andrey sends it or he does
not. A draft is the finished act for you.

Never resolve a review thread you are not certain a bot opened. Uncertain is human.

Everything you claim must be backed by something you actually observed; cite it. A claim you cannot
cite is a thought, and you say so rather than dress it as a finding.
";

/// The section id, so a test can find the section in an assembled projection by name.
pub fn section_id() -> SectionId {
    SectionId::new("boundary")
}

/// The block, for anything that prepends rather than projects.
pub fn block() -> &'static str {
    BOUNDARY_BLOCK
}

/// The section's title. The BODY is [`BOUNDARY_BLOCK`] and nothing else, byte for byte: a title
/// woven into the text would be a second spelling of the boundary.
pub const SECTION_TITLE: &str = "Write boundary";

/// The renderer: a constant. It reads no ledger, so `as_of` cannot change it and a reconstructed
/// past request carries the same boundary the live one did.
pub struct BoundarySection;

#[async_trait::async_trait]
impl SectionRender for BoundarySection {
    async fn render(
        &self,
        _req: &SectionRequest,
    ) -> Result<Option<SectionBody>, bough_plugin_projection::ProjectionError> {
        Ok(Some(SectionBody {
            title: SECTION_TITLE.to_string(),
            body: BOUNDARY_BLOCK.to_string(),
            cites: SectionCites::default(),
        }))
    }
}

/// The one spec this row contributes. `Global`, so it reaches residents AND workers; `Never`, so
/// no degradation rung can take it away.
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
        render: Arc::new(BoundarySection),
    }
}

/// No configuration: the boundary is not a deployment's to vary (§0.2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundaryConfig {}

/// The row.
pub struct BoundaryPlugin;

#[async_trait::async_trait]
impl Plugin for BoundaryPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = BoundaryConfig;

    /// `ledger` is OPTIONAL and DECLARED: the runtime invariant lists the agents it re-assembles
    /// for. Without it the check is vacuous rather than a capability escape (§0.3).
    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["projection"])
            .union(&bough_kernel::Inject::optional(["ledger"]))
    }

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    /// Register ONE global section: `Position { slot: Slot::Identity, place: Place::After }`,
    /// `SectionScope::Global`, `DropPriority::Never` — a buildable wake without the boundary is
    /// worse than no wake — rendering [`BOUNDARY_BLOCK`] verbatim. WP-4.
    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(entry, e))?;
        // A REGISTRATION IS AN EFFECT: unloading this row removes the section and leaves the
        // registry as if the boundary had never mounted.
        projection.section(&ctx, section_spec()).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(BoundaryPlugin);

#[cfg(test)]
mod tests {
    /// P6-D3: the spawner's block must keep stating what this block states. It is a different
    /// sentence today, and this test is what stops the two drifting apart before the merge folds
    /// them.
    #[test]
    fn the_spawner_block_states_the_same_refusals() {
        for needle in ["pull request", "Linear", "bot thread", "Cite the"] {
            assert!(
                bough_plugin_worker_spawn::WRITE_BOUNDARY.contains(needle),
                "the spawner's WRITE_BOUNDARY stopped saying `{needle}`"
            );
        }
    }
}
