//! Invariant: this crate is the projection SERVICE DEFINITION (§0.2, P1-D1). Context IS a
//! projection of the ledger (§5): deterministic, no LLM in the request path, a fixed section
//! order, and degradation in a fixed reverse order that is never silent for pins, digest or mail.
//! This crate owns the key, the vocabulary and the three pure algorithms every provider shares;
//! it has no `Plugin` impl and no bundle row.
//!
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
        let projector = self.0.clone();
        let entry = ctx.entry_id().clone();
        ctx.effect(move |e| async move {
            let token = projector
                .section(spec)
                .map_err(|err| PluginError::new(entry, anyhow::Error::new(err)))?;
            // Disposal removes the section, so unloading the contributor leaves the registry as if
            // it had never mounted (§0.2).
            e.defer_sync(move || token.remove());
            Ok(())
        })
        .await
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

impl Flag {
    /// The word this flag contributes to the `> DEGRADED:` line.
    pub fn word(&self) -> &'static str {
        match self {
            Flag::PinsDegraded => "pins",
            Flag::MailDegraded => "mail",
            Flag::DigestDegraded => "digest",
            Flag::OverBudget => "over-budget",
        }
    }
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
        let mut out = String::new();
        if !self.flags.is_empty() {
            let words: Vec<&str> = self.flags.iter().map(Flag::word).collect();
            out.push_str("> DEGRADED: ");
            out.push_str(&words.join(", "));
            out.push_str("\n\n");
        }
        for s in &self.sections {
            out.push_str("## ");
            out.push_str(&s.title);
            out.push_str("\n\n");
            out.push_str(&s.body);
            out.push_str("\n\n");
        }
        while out.ends_with('\n') {
            out.pop();
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::{Place, Position, Slot};

    fn sec(title: &str, body: &str) -> RenderedSection {
        RenderedSection {
            id: SectionId::new(title),
            position: Position {
                slot: Slot::Identity,
                place: Place::Band,
            },
            title: title.into(),
            body: body.into(),
            cites: SectionCites::default(),
            tokens: 0,
            degraded: None,
        }
    }

    fn assembled(flags: &[Flag]) -> Assembled {
        Assembled {
            agent: AgentName::new("sol"),
            sections: vec![sec("Identity", "sol / lane/sol"), sec("Pins", "- a pin")],
            flags: flags.iter().copied().collect(),
            tokens: 0,
            budget: 100,
            cites: SectionCites::default(),
        }
    }

    #[test]
    fn to_text_is_headers_and_bodies_and_nothing_else() {
        assert_eq!(
            assembled(&[]).to_text(),
            "## Identity\n\nsol / lane/sol\n\n## Pins\n\n- a pin\n"
        );
    }

    /// §5: pins, digest and mail never degrade SILENTLY.
    #[test]
    fn degradation_shows_up_as_a_leading_flag_line() {
        let text = assembled(&[Flag::MailDegraded, Flag::PinsDegraded]).to_text();
        assert!(
            text.starts_with("> DEGRADED: pins, mail\n\n"),
            "flags render in their fixed order, whatever order they were raised in: {text}"
        );
    }
}
