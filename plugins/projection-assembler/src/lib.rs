//! Invariant: this is the projection PROVIDER (§0.2). Context IS a projection of the ledger (§5):
//! this crate assembles it deterministically — no LLM in the request path — and degrades it in a
//! fixed order that is never silent for pins, digest or mail. It injects `ledger` and provides
//! `projection`; its bundle row is `projection-assembler`.

pub mod assemble;
pub mod bands;
pub mod degrade;
pub mod invariant;
pub mod registry;
pub mod resolve;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{Ledger, LedgerHandle};
use bough_plugin_projection::{
    file_view, AssembleRequest, Assembled, FileViewRequest, Projection, ProjectionError,
    ProjectionHandle, Projector, SectionSpec, SectionToken,
};

use crate::registry::Registry;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "projection-assembler";

/// The row's config. Every deployment-varying number §5 names is a validated field here, never a
/// constant in the code (AGENTS.md).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssemblerConfig {
    /// The model's context window in tokens, before headroom.
    pub budget_tokens: usize,
    /// §5's headroom factor. 0.6 until a live measurement moves it (P1-D20).
    pub headroom: f32,
    /// How many steps the verbatim tail selects.
    pub tail_steps: usize,
    /// The floor §5 names: rung 2 never shrinks the tail below this.
    pub tail_floor_steps: usize,
    /// The "newest N" a collapsed mail header keeps.
    pub mail_newest_n: usize,
    /// Tiers above this are never rendered.
    pub max_tiers: u8,
    /// Where `write_file_view` puts a rendered trajectory.
    pub file_view_dir: PathBuf,
}

impl AssemblerConfig {
    /// PURE validation (§0.5): `0.0 < headroom <= 1.0`, `tail_floor_steps <= tail_steps`,
    /// `budget_tokens > 0`, `mail_newest_n > 0`. Anything else is a bundle typo and fails loud at
    /// compose.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if !(self.headroom > 0.0 && self.headroom <= 1.0) {
            return reject(format!(
                "headroom must be in (0.0, 1.0]; got {}",
                self.headroom
            ));
        }
        if self.tail_floor_steps > self.tail_steps {
            return reject(format!(
                "tail_floor_steps ({}) must not exceed tail_steps ({})",
                self.tail_floor_steps, self.tail_steps
            ));
        }
        if self.budget_tokens == 0 {
            return reject("budget_tokens must be greater than zero".to_string());
        }
        if self.mail_newest_n == 0 {
            return reject("mail_newest_n must be greater than zero".to_string());
        }
        Ok(())
    }
}

/// The projector behind the `projection` binding.
pub struct Assembler {
    pub(crate) cfg: Arc<AssemblerConfig>,
    pub(crate) ledger: LedgerHandle,
    pub(crate) registry: Arc<Registry>,
    /// The provider's captured context: the `projection/assemble` waterfall dispatches from it.
    pub(crate) ctx: Context,
}

impl Assembler {
    /// Build an assembler over an injected ledger.
    pub fn new(cfg: Arc<AssemblerConfig>, ledger: LedgerHandle, ctx: Context) -> Arc<Assembler> {
        Arc::new(Assembler {
            cfg,
            ledger,
            registry: Arc::new(Registry::default()),
            ctx,
        })
    }

    /// Where a file view lands when the caller names no directory.
    pub fn file_view_dir(&self) -> &Path {
        &self.cfg.file_view_dir
    }
}

#[async_trait::async_trait]
impl Projector for Assembler {
    fn provider(&self) -> &'static str {
        AssemblerPlugin::NAME
    }
    fn section(&self, spec: SectionSpec) -> Result<SectionToken, ProjectionError> {
        self.registry.add(spec)
    }
    async fn assemble(&self, req: &AssembleRequest) -> Result<Assembled, ProjectionError> {
        crate::assemble::assemble(self, req).await
    }
    async fn file_view(&self, req: &FileViewRequest) -> Result<String, ProjectionError> {
        let view = self.ledger.0.trajectory_view(&req.traj).await?;
        Ok(file_view::render_file_view(&view, req.at))
    }
    async fn write_file_view(
        &self,
        req: &FileViewRequest,
        dir: Option<&Path>,
    ) -> Result<PathBuf, ProjectionError> {
        let text = self.file_view(req).await?;
        // Defaults and the file NAME are resolved in one explicit step (§0.2); a traj id never
        // becomes a path.
        let spec = crate::resolve::resolve_file_view(req, &self.cfg, dir)?;
        let path = spec.path();
        let fail = |detail: String| ProjectionError::FileView {
            path: path.display().to_string(),
            detail,
        };
        std::fs::create_dir_all(&spec.dir).map_err(|e| fail(e.to_string()))?;
        std::fs::write(&path, text.as_bytes()).map_err(|e| fail(e.to_string()))?;
        Ok(path)
    }
}

/// The provider plugin.
pub struct AssemblerPlugin;

#[async_trait::async_trait]
impl Plugin for AssemblerPlugin {
    const NAME: &'static str = "projection-assembler";
    type Config = AssemblerConfig;

    fn inject() -> Inject {
        // The typed key names itself: a rename on the Definition is a compile error here, not a
        // boot failure (§13).
        Inject::required([<Ledger as bough_kernel::ServiceKey>::NAME])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        cfg.validate()
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // The recorded citation stream is per-LIFE: a RELOAD keeps the `FiberUid`, so this
        // fiber's observations are forgotten when it unloads (§0.3, and `hello`'s precedent).
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || bough_plugin_projection::invariant::forget(mine));
            Ok(())
        })
        .await?;

        let assembler = Assembler::new(cfg, LedgerHandle(ledger.0.clone()), ctx.clone());
        ctx.provide::<Projection>(ProjectionHandle(assembler))
            .await
            .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(AssemblerPlugin);

#[cfg(test)]
mod config_tests {
    use super::*;

    fn ok() -> AssemblerConfig {
        crate::test_support::cfg_small()
    }

    #[test]
    fn a_headroom_outside_zero_to_one_is_rejected() {
        let mut c = ok();
        c.headroom = 0.0;
        assert!(c.validate().is_err());
        c.headroom = 1.5;
        assert!(c.validate().is_err());
        c.headroom = 1.0;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn a_floor_above_the_tail_is_a_bundle_typo() {
        let mut c = ok();
        c.tail_floor_steps = c.tail_steps + 1;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("tail_floor_steps"), "{err}");
    }

    #[test]
    fn a_zero_budget_or_zero_mail_n_is_rejected() {
        let mut c = ok();
        c.budget_tokens = 0;
        assert!(c.validate().is_err());
        let mut c = ok();
        c.mail_newest_n = 0;
        assert!(c.validate().is_err());
    }
}

/// Fixtures every unit test in this crate shares. Hermetic: a memory ledger, a root context, a
/// fixed `at`, and rows built through the real append path wherever the behaviour under test is
/// the PROVIDER's (pin supersession, mail consumption) rather than the renderer's.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Arc;

    use bough_kernel::{Context, KernelCore};
    use bough_plugin_ledger::{
        AgentName, AgentRow, Append, Cite, Class, LedgerHandle, Pin, Ref, Rollup, RollupKind, Seq,
        Step, StepId, StepType, TrajId, WakeId,
    };
    use bough_plugin_ledger_memory::store::MemoryStore;
    use bough_plugin_projection::AssembleRequest;
    use chrono::{DateTime, TimeZone, Utc};

    use crate::{Assembler, AssemblerConfig};

    /// A fixed instant. Nothing in the request path reads a clock, so every test states its own.
    pub fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
    }

    /// A config whose numbers are small enough that a degradation test can state a budget by hand.
    pub fn cfg_small() -> AssemblerConfig {
        AssemblerConfig {
            budget_tokens: 1_000,
            headroom: 1.0,
            tail_steps: 20,
            tail_floor_steps: 5,
            mail_newest_n: 3,
            max_tiers: 3,
            file_view_dir: std::path::PathBuf::from("/nonexistent-unless-a-test-writes"),
        }
    }

    pub fn assemble_request(agent: &str) -> AssembleRequest {
        AssembleRequest {
            agent: AgentName::new(agent),
            wake: None,
            at: at(),
            budget: None,
        }
    }

    /// A step built directly, for the renderers that are pure functions of rows.
    pub fn step(id: &str, seq: u64, wake: &str) -> Step {
        Step {
            id: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            at: at(),
            wake: WakeId::new(wake),
            kind: StepType::new("probe/note"),
            class: Class::Thought,
            body: Arc::new(serde_json::json!({ "note": id })),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    /// A `mail/delivered` row built directly.
    pub fn mail_step(id: &str, seq: u64, class: &str, subject: &str) -> Step {
        Step {
            kind: StepType::new(crate::bands::MAIL_DELIVERED),
            class: Class::Evidence,
            body: Arc::new(serde_json::json!({
                "class": class,
                "from": "someone",
                "subject": subject,
                "summary": subject,
                "refs": [],
            })),
            ..step(id, seq, "w1")
        }
    }

    pub fn pin(id: &str, seq: u64, title: &str, text: &str) -> Pin {
        Pin {
            step: StepId::new(id),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            class: Class::Thought,
            title: title.to_string(),
            text: text.to_string(),
        }
    }

    pub fn tier_rollup(id: &str, tier: u8, from: u64, to: u64, notable: &[&str]) -> Rollup {
        Rollup {
            id: bough_plugin_ledger::RollupId::new(id),
            traj: TrajId::new("t1"),
            kind: RollupKind::Tier,
            tier,
            from_seq: Seq(from),
            to_seq: Seq(to),
            src_trajs: vec![TrajId::new("t1")],
            body: serde_json::Value::String(id.to_string()),
            notable_refs: notable.iter().map(Ref::new).collect(),
            prompt_ver: "p1".to_string(),
            sealed_at: at(),
            superseded_by: None,
        }
    }

    /// A live memory ledger and an assembler over it.
    pub struct Fixture {
        pub ctx: Context,
        pub ledger: LedgerHandle,
        pub traj: TrajId,
        assembler: Arc<Assembler>,
    }

    impl Fixture {
        pub async fn memory() -> Fixture {
            let ctx = Context::root(KernelCore::new());
            let store = MemoryStore::new(ctx.clone());
            let ledger = LedgerHandle(store);
            let assembler = Assembler::new(Arc::new(cfg_small()), ledger.clone(), ctx.clone());
            Fixture {
                ctx,
                ledger,
                traj: TrajId::new("t1"),
                assembler,
            }
        }

        pub fn assembler(&self) -> Arc<Assembler> {
            self.assembler.clone()
        }

        pub async fn seed_agent(&self) {
            self.ledger
                .0
                .put_agent(AgentRow {
                    name: AgentName::new("sol"),
                    traj: self.traj.clone(),
                    routing_refs: BTreeSet::new(),
                    wake_classes: BTreeSet::new(),
                    model_override: None,
                    tick_floor: None,
                    digest_rollup: None,
                })
                .await
                .expect("agents is mutable config");
        }

        async fn append(
            &self,
            kind: &str,
            class: Class,
            body: serde_json::Value,
            cites: Vec<Cite>,
        ) -> Step {
            self.ledger
                .0
                .append(Append {
                    traj: self.traj.clone(),
                    wake: WakeId::new("w1"),
                    kind: StepType::new(kind),
                    class,
                    body,
                    cites,
                    at: at(),
                    id: None,
                })
                .await
                .expect("the fixture only appends rows the builtin schemas accept")
        }

        pub async fn pin_set(&self, title: &str, text: &str, supersedes: &[StepId]) -> StepId {
            self.append(
                "pin/set",
                Class::Thought,
                serde_json::json!({
                    "title": title,
                    "text": text,
                    "supersedes": supersedes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                }),
                Vec::new(),
            )
            .await
            .id
        }

        pub async fn pin_retire(&self, retires: &[StepId], reason: &str) -> StepId {
            self.append(
                "pin/retire",
                Class::Thought,
                serde_json::json!({
                    "retires": retires.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    "reason": reason,
                }),
                Vec::new(),
            )
            .await
            .id
        }

        pub async fn mail(&self, from: &str, subject: &str) -> Step {
            self.append(
                crate::bands::MAIL_DELIVERED,
                Class::Evidence,
                serde_json::json!({
                    "class": "ordinary",
                    "from": from,
                    "subject": subject,
                    "summary": subject,
                    "refs": [],
                }),
                vec![Cite {
                    r#ref: Ref::new(from),
                    url: None,
                }],
            )
            .await
        }

        /// Close a wake whose `consumed` set names exactly these rows' seqs.
        pub async fn close_wake_consuming(&self, consumed: &[Step]) {
            let ranges: Vec<serde_json::Value> = consumed
                .iter()
                .map(|s| serde_json::json!({ "from": s.seq.0, "to": s.seq.0 }))
                .collect();
            self.append(
                "wake/end",
                Class::Thought,
                serde_json::json!({ "reason": "completed", "cause": null, "consumed": ranges }),
                Vec::new(),
            )
            .await;
        }

        pub async fn live_pins(&self) -> Vec<Pin> {
            let mut p = self
                .ledger
                .0
                .live_pins(std::slice::from_ref(&self.traj))
                .await
                .expect("live_pins is a read");
            crate::bands::sort_pins(&mut p);
            p
        }

        pub async fn unconsumed(&self) -> Vec<Step> {
            self.ledger
                .0
                .unconsumed_mail(&self.traj)
                .await
                .expect("unconsumed_mail is a read")
        }
    }

    /// Kept so an unused-import warning names the file rather than the prelude.
    #[allow(dead_code)]
    pub type Unused = HashMap<(), ()>;
}
