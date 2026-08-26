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
use bough_plugin_projection::SectionId;

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

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["projection"])
    }

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    /// Register ONE global section: `Position { slot: Slot::Identity, place: Place::After }`,
    /// `SectionScope::Global`, `DropPriority::Never` — a buildable wake without the boundary is
    /// worse than no wake — rendering [`BOUNDARY_BLOCK`] verbatim. WP-4.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4")
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
