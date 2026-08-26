//! Invariant (P5-D11 again, for an ordinary lane): every registration is made from THIS ROW's ctx
//! and scoped to its lane by spec, so a patch that drops a lane from the list unwinds exactly that
//! lane's section and restriction and nothing else.
//!
//! P5-D17: the GLOBAL persona section is this row's too. Shadowing is only demonstrable against a
//! twin, and no row contributed a persona section before this phase — without one,
//! "most-specific-wins" has nothing to win against and V6 could only assert that a section
//! appeared. Keeping both halves in one row keeps the pair honest: same `SectionId`, one place to
//! read the rule.
//!
//! A lane named by config with no live agent is a WARNING at apply and a RETRY on `agent/created`,
//! never a boot failure: lanes are born at runtime, and a config that names tomorrow's lane must
//! not stop today's boot.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "lane-scope";

/// The `SectionId` both the global persona and every lane's persona are contributed under. One id
/// is what makes the lane's section SHADOW the global one rather than sit beside it.
pub const PERSONA_SECTION_ID: &str = "persona";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneScopeConfig {
    /// The GLOBAL persona section. `None` ⇒ no global section, and a lane's persona is then
    /// simply additive (P5-D17).
    pub default_persona: Option<String>,
    /// One entry per lane that wants a scoped world.
    pub lanes: Vec<LaneSpec>,
}

/// One lane's scoped world.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneSpec {
    pub agent: String,
    /// Replaces the global persona section FOR THIS AGENT (same `SectionId`, agent scope).
    pub persona: Option<String>,
    /// §5's intersection filter. `None` ⇒ everything the deny list admits.
    pub allow: Option<Vec<String>>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// PURE: the [`bough_plugin_tools::Restrict`] one [`LaneSpec`] asks for.
pub fn restrict_of(_spec: &LaneSpec) -> bough_plugin_tools::Restrict {
    todo!("WP-5: allow/deny to Restrict, with ToolName branding at the boundary")
}

/// The `lane.scope` row.
pub struct LaneScopePlugin;

#[async_trait::async_trait]
impl Plugin for LaneScopePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LaneScopeConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "tools", "projection"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!(
            "WP-5: the global section, then per-lane section + restrict, then the \
               agent/created retry listener"
        )
    }

    fn invariants() -> Vec<InvariantSpec> {
        // See `invariant.rs`: no runtime invariant, and why.
        Vec::new()
    }
}

bough_kernel::register_plugin!(LaneScopePlugin);
