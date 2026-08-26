//! Invariant: `projection-probe` is a TEST INSTRUMENT, not a product row (P1-D16). It exists to
//! exercise the REAL catalog path for §17 Phase 1: it injects `ledger` and `projection`, declares
//! two step types (`probe/note`, and `probe/scratch` with `ignorable: true`), registers one global
//! and one agent-scoped section with the SAME `SectionId` (the shadowing fixture), appends a small
//! scripted trajectory on `apply`, and pushes every interesting moment onto a shared trace the
//! tests assert on in order. It is in no bundle; the tests' own `$BOUGH_HOME` mounts it.
//!
//! SCAFFOLD: `unused_variables` and `dead_code` are allowed while the bodies are `todo!()` and the
//! private state they thread has no reader yet. Both allows go away with the last `todo!()`.
#![allow(unused_variables, dead_code)]

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "projection-probe";

/// The probe's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    /// The trajectory the scripted steps are appended to.
    pub traj: String,
    /// The agent the probe's agent-scoped section shadows a global one for.
    pub agent: String,
    /// How many scripted steps `apply` appends.
    #[serde(default = "default_steps")]
    pub steps: usize,
}

fn default_steps() -> usize {
    3
}

/// One recorded moment, in the `hello` trace tradition.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceLine {
    pub plugin: &'static str,
    pub moment: String,
}

/// Everything the probe has done this process, in order.
pub fn trace() -> Vec<TraceLine> {
    todo!("WP-6: projection-probe trace")
}

/// Push one moment onto the shared trace.
pub fn push(plugin: &'static str, moment: impl Into<String>) {
    todo!("WP-6: projection-probe push")
}

/// Drop the trace. Test setup only.
pub fn clear() {
    todo!("WP-6: projection-probe clear")
}

/// The fixture plugin.
pub struct ProbePlugin;

#[async_trait::async_trait]
impl Plugin for ProbePlugin {
    const NAME: &'static str = "projection-probe";
    type Config = ProbeConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "projection"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!(
            "WP-6: ProbePlugin::apply — declare the two step types, register the two sections, \
               append the scripted trajectory, and record every moment on the trace"
        )
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ProbePlugin);
