//! Invariant: this crate is the projection SERVICE DEFINITION (§0.2, P1-D1). Context IS a
//! projection of the ledger (§5): deterministic, no LLM in the request path, a fixed section
//! order, and degradation in a fixed reverse order that is never silent for pins, digest or mail.
//! This crate owns the key, the vocabulary and the three pure algorithms every provider shares;
//! it has no `Plugin` impl and no bundle row.
//!
//! SCAFFOLD: `unused_variables` and `dead_code` are allowed while the bodies are `todo!()` and the
//! private state they thread has no reader yet. Both allows go away with the last `todo!()`.
#![allow(unused_variables, dead_code)]

pub mod error;
pub mod file_view;
pub mod invariant;
pub mod order;
pub mod section;
pub mod tokens;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, PluginError, ServiceKey, WaterfallEvent};
use bough_plugin_ledger::{AgentName, TrajId, WakeId};
use chrono::{DateTime, Utc};

pub use error::ProjectionError;
pub use section::{
    DropPriority, Place, Position, SectionBody, SectionCites, SectionId, SectionRender,
    SectionRequest, SectionScope, SectionSpec, SectionToken, Slot,
};

/// The `projection` service key.
pub struct Projection;

impl ServiceKey for Projection {
    type Value = ProjectionHandle;
    const NAME: &'static str = "projection";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct ProjectionHandle(pub Arc<dyn Projector>);

/// What a projection provider does.
#[async_trait::async_trait]
pub trait Projector: Send + Sync + 'static {
    /// Catalog name of the plugin behind this binding.
    fn provider(&self) -> &'static str;
    /// Register a contributed section. Synchronous, so the effect wrapper owns the disposal.
    fn section(&self, spec: SectionSpec) -> Result<SectionToken, ProjectionError>;
    /// Assemble one agent's context. Deterministic: no LLM, no clock read, no filesystem.
    async fn assemble(&self, req: &AssembleRequest) -> Result<Assembled, ProjectionError>;
    /// Render a trajectory to text. A pure function of the ledger; writes nothing.
    async fn file_view(&self, req: &FileViewRequest) -> Result<String, ProjectionError>;
    /// [`Projector::file_view`] plus one write. Returns the path written.
    async fn write_file_view(
        &self,
        req: &FileViewRequest,
        dir: Option<&Path>,
    ) -> Result<PathBuf, ProjectionError>;
}

impl ProjectionHandle {
    /// §5's `ctx.projection.section()`: an effect, so unloading the contributor removes it.
    pub async fn section(
        &self,
        ctx: &Context,
        spec: SectionSpec,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-4: ProjectionHandle::section")
    }
}

/// One assembly request. `at` is supplied by the caller; the assembler never reads a clock.
#[derive(Clone, Debug)]
pub struct AssembleRequest {
    pub agent: AgentName,
    pub wake: Option<WakeId>,
    pub at: DateTime<Utc>,
    /// `None` ⇒ the configured `budget_tokens`.
    pub budget: Option<usize>,
}

/// A file-view request.
#[derive(Clone, Debug)]
pub struct FileViewRequest {
    pub traj: TrajId,
    pub at: DateTime<Utc>,
}

/// One section, rendered and measured.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedSection {
    pub id: SectionId,
    pub position: Position,
    pub title: String,
    pub body: String,
    pub cites: SectionCites,
    pub tokens: usize,
    /// How this section was cut down, if it was.
    pub degraded: Option<Degradation>,
}

/// What a degradation rung did to a section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Degradation {
    TiersDropped,
    TailShrunk,
    PinsCollapsed,
    MailCollapsed,
    DigestTruncated,
}

/// In-context flags: degradation of pins, digest or mail is NEVER silent (§5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Flag {
    PinsDegraded,
    MailDegraded,
    DigestDegraded,
    /// Still over budget after every rung. Nothing was dropped silently.
    OverBudget,
}

/// One assembled projection.
#[derive(Clone, Debug, PartialEq)]
pub struct Assembled {
    pub agent: AgentName,
    pub sections: Vec<RenderedSection>,
    pub flags: BTreeSet<Flag>,
    pub tokens: usize,
    pub budget: usize,
    /// The union of every surviving section's cites — exactly what the model-visible ⟺ ledgered
    /// invariant reads.
    pub cites: SectionCites,
}

impl Assembled {
    /// THE golden surface: `## <title>\n\n<body>\n` per section, with a leading
    /// `> DEGRADED: pins, mail` line when `flags` is non-empty, and nothing else. No timestamps
    /// and no process ids — the text is a function of (ledger contents, request, config) alone.
    pub fn to_text(&self) -> String {
        todo!("WP-4: Assembled::to_text")
    }
}

/// `projection/assemble` — the waterfall §5 puts around the assembler, dispatched BETWEEN
/// rendering and degradation so a listener may add a section and still be budgeted.
pub struct ProjectionAssemble;

impl WaterfallEvent for ProjectionAssemble {
    const NAME: &'static str = "projection/assemble";
    type Value = Draft;
}

/// The value the waterfall carries.
#[derive(Clone, Debug)]
pub struct Draft {
    pub request: Arc<AssembleRequest>,
    pub sections: Vec<RenderedSection>,
    pub budget: usize,
    pub flags: BTreeSet<Flag>,
}
