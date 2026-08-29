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
use bough_plugin_ledger::{AgentName, Seq, TrajId, WakeId};
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
    /// Pin an already-assembled prefix for ONE agent (P5-D12, §10). [`Projector::assemble`]
    /// returns it verbatim for that agent, whatever the request's budget or `as_of` says, and
    /// records `source` so the request stays reconstructible from the ledger. Synchronous: the
    /// caller wraps it in an effect, so the pin unwinds with the agent that holds it.
    fn pin_prefix(
        &self,
        agent: AgentName,
        prefix: Assembled,
        source: PrefixSource,
    ) -> Result<PrefixToken, ProjectionError>;

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

    /// [`Projector::pin_prefix`] as an EFFECT (P5-D12). The pin unwinds with whatever registered
    /// it — in practice the child agent's setup — so nothing global remembers a fork's prefix
    /// once the fork is gone.
    ///
    /// DEVIATION from plan §2.7, which lists the trait method alone: a Provider lives in another
    /// crate and every other registration on this handle is an effect, so the Definition offers
    /// the wrapper rather than leaving each caller to write it.
    pub async fn pin_prefix(
        &self,
        ctx: &Context,
        agent: AgentName,
        prefix: Assembled,
        source: PrefixSource,
    ) -> Result<EffectHandle, PluginError> {
        let projector = self.0.clone();
        let entry = ctx.entry_id().clone();
        ctx.effect(move |e| async move {
            let token = projector
                .pin_prefix(agent, prefix, source)
                .map_err(|err| PluginError::new(entry, anyhow::Error::new(err)))?;
            e.defer_sync(move || token.remove());
            Ok(())
        })
        .await
    }
}

/// Where a pinned prefix came from. Written durably as `fork/prefix` by `worker-fork`, so §0.2's
/// "the sent request reconstructs from the ledger" survives pinning: re-assembling `of_agent` at
/// `as_of` reproduces the pin.
#[derive(Clone, Debug, PartialEq)]
pub struct PrefixSource {
    pub of_agent: AgentName,
    pub as_of: Seq,
}

/// The disposer for one pinned prefix. Dropping it does NOT unpin; `remove()` does, and the
/// effect wrapper calls it (the [`SectionToken`] precedent).
#[derive(Clone)]
pub struct PrefixToken {
    #[doc(hidden)]
    inner: Arc<dyn Fn() + Send + Sync>,
}

impl PrefixToken {
    /// A token over a provider's own unpin closure.
    pub fn new(remove: impl Fn() + Send + Sync + 'static) -> PrefixToken {
        PrefixToken {
            inner: Arc::new(remove),
        }
    }
    /// Unpin. Idempotent by the provider's contract.
    pub fn remove(&self) {
        (self.inner)()
    }
}

impl std::fmt::Debug for PrefixToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrefixToken")
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
    /// The ledger high-water to assemble AT (§2.7 item 3, P2-D20). `None` ⇒ now.
    ///
    /// Every row above it is invisible: to the six built-in bands AND to every contributed
    /// section, which is handed the same value. Without that second half a reconstruction is only
    /// as good as the sections nobody contributed.
    pub as_of: Option<Seq>,
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
        self.render(&self.sections, true)
    }

    /// The projection as the TWO prompt-cache tiers `bough-llm` was built for (§12): the STABLE
    /// text (every section whose slot precedes [`crate::section::Slot::Tail`] — identity, pins,
    /// digest, tiers) and the VOLATILE text (the tail band and mail, which move every wake).
    ///
    /// The split is BY SLOT, not by measurement: the tail and mail bands change on every wake by
    /// construction, so keeping them out of the stable tier is what lets the provider's cache
    /// re-read the identity/pins/digest/tiers prefix across wakes instead of rewriting it. The
    /// `DEGRADED` flags line stays on the STABLE half deliberately: degradation is the exception,
    /// and hiding it from the tier the model anchors on would soften §5's "never silently".
    ///
    /// `to_text()` remains the golden surface (the digest, the reconstruction, the context view);
    /// this is the REQUEST-BUILDING view of the same sections, in the same order.
    pub fn tier_split(&self) -> (String, String) {
        let boundary = self
            .sections
            .iter()
            .position(|s| s.position.slot >= crate::section::Slot::Tail)
            .unwrap_or(self.sections.len());
        (
            self.render(&self.sections[..boundary], true),
            self.render(&self.sections[boundary..], false),
        )
    }

    fn render(&self, sections: &[RenderedSection], with_flags: bool) -> String {
        let mut out = String::new();
        if with_flags && !self.flags.is_empty() {
            let words: Vec<&str> = self.flags.iter().map(Flag::word).collect();
            out.push_str("> DEGRADED: ");
            out.push_str(&words.join(", "));
            out.push_str("\n\n");
        }
        for s in sections {
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

    fn sec_at(slot: Slot, title: &str, body: &str) -> RenderedSection {
        RenderedSection {
            position: Position {
                slot,
                place: Place::Band,
            },
            ..sec(title, body)
        }
    }

    /// §12's cache contract: the tiers split AT the tail band, in section order, and a change
    /// confined to the tail/mail bands leaves the stable text byte-identical.
    #[test]
    fn the_tiers_split_at_the_tail_band_and_a_tail_change_leaves_the_stable_text_alone() {
        let mut a = assembled(&[]);
        a.sections = vec![
            sec_at(Slot::Identity, "Identity", "sol / lane/sol"),
            sec_at(Slot::Tiers, "Tier 1", "a summary"),
            sec_at(Slot::Tail, "Recent steps", "andrey: hello"),
            sec_at(Slot::Mail, "Mail", "- one item"),
        ];
        let (stable, volatile) = a.tier_split();
        assert_eq!(
            stable,
            "## Identity\n\nsol / lane/sol\n\n## Tier 1\n\na summary\n"
        );
        assert_eq!(
            volatile,
            "## Recent steps\n\nandrey: hello\n\n## Mail\n\n- one item\n"
        );

        let mut b = a.clone();
        b.sections[2].body = "andrey: hello\nsol: hi".into();
        let (stable_b, volatile_b) = b.tier_split();
        assert_eq!(
            stable, stable_b,
            "a tail-only change never moves the stable tier"
        );
        assert_ne!(volatile, volatile_b);
    }

    /// A projection with no tail/mail is all stable; the flags line rides the STABLE half.
    #[test]
    fn an_all_stable_projection_has_an_empty_volatile_half_and_flags_stay_stable() {
        let a = assembled(&[Flag::PinsDegraded]);
        let (stable, volatile) = a.tier_split();
        assert_eq!(stable, a.to_text());
        assert_eq!(volatile, "");
        assert!(stable.starts_with("> DEGRADED: pins\n\n"), "{stable}");
    }
}
