//! Invariant: this crate is the `js` SERVICE DEFINITION (phase codemode §2.1). It owns the `js`
//! key, the `Program`/`HostFn`/`Caps` vocabulary and the engine factory slot — and not one line
//! of engine code, no I/O of its own, and no domain vocabulary. Everything a program can reach
//! is in `Program::host`.

pub mod engine;
pub mod error;
pub mod invariant;
pub mod program;

use std::sync::Arc;

use bough_kernel::{
    Context, EffectHandle, FiberUid, InvariantSpec, Plugin, PluginError, ServiceKey,
};

pub use engine::JsEngine;
pub use error::JsError;
pub use program::{Caps, ConsoleSink, HostCall, HostFn, HostRefusal, Program, RefusalKind, Run};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "js";

/// The `js` service key.
pub struct Js;

impl ServiceKey for Js {
    type Value = JsHandle;
    const NAME: &'static str = "js";
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct JsHandle(pub Arc<JsInner>);

/// The seam's live state: the one engine, and the caps a program gets when it names none.
pub struct JsInner {
    engine: parking_lot::Mutex<Option<Arc<dyn JsEngine>>>,
    default_caps: Caps,
    /// The fiber whose life this handle's invariant observations belong to. `None` until the row
    /// attaches it in `apply`; a hand-built handle in a test simply has none.
    owner: parking_lot::Mutex<Option<FiberUid>>,
}

impl JsHandle {
    /// No `new()` and no `Default`: `Caps` are deployment-varying and [`JsConfig`] is their one
    /// source (§0.2), exactly as `ToolsHandle::with_limits` is spelled.
    pub fn with_caps(default: Caps) -> JsHandle {
        JsHandle(Arc::new(JsInner {
            engine: parking_lot::Mutex::new(None),
            default_caps: default,
            owner: parking_lot::Mutex::new(None),
        }))
    }

    /// The factory slot, in the shape of `ctx.agents.set_factory` (§2): a SECOND engine is an
    /// ERROR, not a silent replacement. Registration is an effect; the disposer clears the slot.
    ///
    pub async fn set_engine(
        &self,
        ctx: &Context,
        e: Arc<dyn JsEngine>,
    ) -> Result<EffectHandle, PluginError> {
        {
            let mut slot = self.0.engine.lock();
            if let Some(held) = slot.as_ref() {
                return Err(PluginError::new(
                    ctx.entry_id().clone(),
                    anyhow::anyhow!(
                        "a second JS engine (`{}`) was offered to the `js` seam; `{}` already \
                         holds it. One engine per tree: disable a row instead of stacking them.",
                        e.name(),
                        held.name()
                    ),
                ));
            }
            *slot = Some(e.clone());
        }
        let inner = self.0.clone();
        let mine = e;
        ctx.effect(move |eff| async move {
            eff.defer_sync(move || {
                let mut slot = inner.engine.lock();
                // Only free the slot if it is still OURS: a later taker is not ours to evict.
                if slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, &mine)) {
                    *slot = None;
                }
            });
            Ok(())
        })
        .await
        .map_err(|err| PluginError::new(ctx.entry_id().clone(), err))
    }

    /// Attach the providing row's fiber, so this handle's invariant observations are forgotten
    /// with that fiber's life. Called once by [`JsPlugin::apply`].
    pub fn attach_fiber(&self, fiber: FiberUid) {
        *self.0.owner.lock() = Some(fiber);
    }

    /// The installed engine, if a Provider row set one.
    pub fn engine(&self) -> Option<Arc<dyn JsEngine>> {
        self.0.engine.lock().clone()
    }

    /// The caps a `Program` gets when its caller names none.
    pub fn default_caps(&self) -> Caps {
        self.0.default_caps
    }

    /// Compile-only. The parse comes from the engine that will RUN the program, so host and
    /// engine can never disagree about what is legal (main's `check` message).
    ///
    pub async fn check(&self, src: &str, caps: Caps) -> Result<(), JsError> {
        self.check_bound(src, caps, &[]).await
    }

    /// Compile-only, told which globals the program will be given.
    ///
    /// This is the spelling a CONSUMER should use: it knows the roster it is about to inject, and
    /// only a parse that knows it can say "`bash` is already bound in every program's scope"
    /// instead of a bare `SyntaxError`.
    pub async fn check_bound(
        &self,
        src: &str,
        caps: Caps,
        bound: &[String],
    ) -> Result<(), JsError> {
        let engine = self.engine().ok_or(JsError::NoEngine)?;
        engine.check_bound(src, caps, bound).await
    }

    /// Run one program to its single terminal outcome.
    ///
    pub async fn run(&self, p: Program) -> Result<Run, JsError> {
        let engine = self.engine().ok_or(JsError::NoEngine)?;
        let cancelled = p.cancel.clone();
        let digest = program::digest(&p.source);
        let fiber = self.0.owner.lock().unwrap_or(FiberUid(0));
        let caps = p.caps;
        let out = engine.run(p).await;
        invariant::record(invariant::Obs {
            fiber,
            program: digest,
            ran: out.is_ok(),
            errored: out.is_err(),
            cancelled: cancelled.is_cancelled(),
            // Recorded from the `Run` the engine built, not from the shape of the `Result`: this
            // is the one thing in the observation the engine can actually get wrong.
            cost: out.as_ref().ok().map(|r| invariant::Cost {
                ops: r.ops,
                ms: r.ms,
                ops_cap: caps.ops,
                wall_ms: caps.wall_ms,
            }),
        });
        out
    }
}

/// The row's config: the one source of the default caps.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JsConfig {
    pub default_caps: Caps,
}

/// The Service Definition row.
pub struct JsPlugin;

#[async_trait::async_trait]
impl Plugin for JsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = JsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::none()
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        let c = &cfg.default_caps;
        if c.ops == 0 || c.memory_bytes == 0 || c.stack_bytes == 0 || c.wall_ms == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "every cap must be at least 1: a zero cap is a program that cannot run"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        // The invariant's record is per-LIFE: a reload keeps the `FiberUid`, so this fiber's
        // observations are forgotten when it unloads.
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(mine));
            Ok(())
        })
        .await
        .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        let handle = JsHandle::with_caps(cfg.default_caps);
        handle.attach_fiber(mine);
        ctx.provide::<Js>(handle)
            .await
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::a_cancelled_program_never_reports_a_run()]
    }
}

bough_kernel::register_plugin!(JsPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::KernelCore;

    struct Fake(&'static str);

    #[async_trait::async_trait]
    impl JsEngine for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        async fn check(&self, _src: &str, _caps: Caps) -> Result<(), JsError> {
            Ok(())
        }
        async fn run(&self, _p: Program) -> Result<Run, JsError> {
            Ok(Run {
                console: String::new(),
                console_bytes_dropped: 0,
                ops: 0,
                ms: 0,
                value: None,
            })
        }
    }

    fn caps() -> Caps {
        Caps {
            ops: 1_000,
            memory_bytes: 1 << 20,
            stack_bytes: 1 << 18,
            wall_ms: 1_000,
            console_bytes: 4_096,
        }
    }

    fn ctx() -> Context {
        Context::root(KernelCore::new())
    }

    #[tokio::test]
    async fn a_second_engine_is_an_error_and_the_first_survives() {
        let ctx = ctx();
        let h = JsHandle::with_caps(caps());
        let _first = h
            .set_engine(&ctx, Arc::new(Fake("quickjs")))
            .await
            .expect("the first engine installs");
        let err = match h.set_engine(&ctx, Arc::new(Fake("sidecar"))).await {
            Err(e) => e,
            Ok(_) => panic!("a SECOND engine must be refused, not silently swapped"),
        };
        let msg = err.to_string() + &err.source.to_string();
        assert!(msg.contains("sidecar") && msg.contains("quickjs"), "{msg}");
        assert_eq!(
            h.engine().expect("the first engine survives").name(),
            "quickjs"
        );
    }

    #[tokio::test]
    async fn disposing_the_effect_empties_the_slot() {
        let ctx = ctx();
        let h = JsHandle::with_caps(caps());
        let eff = h.set_engine(&ctx, Arc::new(Fake("quickjs"))).await.unwrap();
        assert!(h.engine().is_some());
        eff.dispose().await;
        assert!(
            h.engine().is_none(),
            "unload must leave no trace of the engine"
        );
        // …and the slot is free for the next Provider.
        h.set_engine(&ctx, Arc::new(Fake("sidecar")))
            .await
            .expect("the slot is free again");
        assert_eq!(h.engine().unwrap().name(), "sidecar");
    }

    #[tokio::test]
    async fn no_engine_is_a_named_error_not_a_panic() {
        let h = JsHandle::with_caps(caps());
        assert_eq!(h.check("1+1", caps()).await, Err(JsError::NoEngine));
    }

    struct Sink;
    impl ConsoleSink for Sink {
        fn write(&self, _line: &str) {}
    }

    fn program(src: &str, cancel: tokio_util::sync::CancellationToken) -> Program {
        Program {
            source: src.to_string(),
            caps: caps(),
            host: vec![],
            console: Arc::new(Sink),
            cancel,
        }
    }

    /// A `Run` reported with a cost past its caps is caught THROUGH THE SEAM.
    ///
    /// The pure `evaluate` cases hand-build an `Obs`; this one proves the wiring — that
    /// `JsHandle::run` records what the engine actually reported, not a restatement of the
    /// `Result`'s shape. An engine whose interrupt handler is wrong returns output for a program
    /// that overran, and the consumer would ledger that console as the round's clean answer.
    #[tokio::test]
    async fn an_engine_that_returns_a_run_past_its_caps_is_caught_by_the_invariant() {
        struct Overrun;
        #[async_trait::async_trait]
        impl JsEngine for Overrun {
            fn name(&self) -> &'static str {
                "overrun"
            }
            async fn check(&self, _src: &str, _caps: Caps) -> Result<(), JsError> {
                Ok(())
            }
            async fn run(&self, p: Program) -> Result<Run, JsError> {
                Ok(Run {
                    console: String::new(),
                    console_bytes_dropped: 0,
                    // Past BOTH caps, and still an `Ok`.
                    ops: p.caps.ops + 1,
                    ms: p.caps.wall_ms + 1,
                    value: None,
                })
            }
        }

        let ctx = ctx();
        let h = JsHandle::with_caps(caps());
        h.set_engine(&ctx, Arc::new(Overrun)).await.unwrap();
        let src = "overrun-case";
        h.run(program(src, Default::default()))
            .await
            .expect("the engine reported success");

        let mine: Vec<invariant::Obs> = invariant::seen()
            .into_iter()
            .filter(|o| o.program == program::digest(src))
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "the seam records one observation per program"
        );
        let detail = invariant::evaluate(&mine).expect_err("the breach must be reported");
        assert!(detail.contains("cap breach is a JsError"), "{detail}");
    }

    /// …and a cancelled program that still reports a `Run` is caught the same way.
    #[tokio::test]
    async fn a_cancelled_program_that_reports_a_run_is_caught_by_the_invariant() {
        let ctx = ctx();
        let h = JsHandle::with_caps(caps());
        h.set_engine(&ctx, Arc::new(Fake("quickjs"))).await.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let src = "cancelled-case";
        h.run(program(src, cancel)).await.expect("the fake returns");

        let mine: Vec<invariant::Obs> = invariant::seen()
            .into_iter()
            .filter(|o| o.program == program::digest(src))
            .collect();
        let detail = invariant::evaluate(&mine).expect_err("cancel + Run must be reported");
        assert!(detail.contains("cancelled"), "{detail}");
    }

    #[test]
    fn caps_round_trip_through_schemars_and_serde() {
        let schema = schemars::schema_for!(Caps);
        let json = serde_json::to_value(&schema).unwrap();
        let props = json["properties"].as_object().expect("an object schema");
        for field in [
            "ops",
            "memory_bytes",
            "stack_bytes",
            "wall_ms",
            "console_bytes",
        ] {
            assert!(props.contains_key(field), "{field} is missing from {json}");
        }
        let back: Caps = serde_json::from_value(serde_json::to_value(caps()).unwrap()).unwrap();
        assert_eq!(back, caps());
        // `deny_unknown_fields`: a typo in a bundle patch fails loud.
        let bad = serde_json::json!({
            "ops": 1, "memory_bytes": 1, "stack_bytes": 1, "wall_ms": 1,
            "console_bytes": 1, "opz": 9
        });
        assert!(serde_json::from_value::<Caps>(bad).is_err());
    }

    #[test]
    fn js_error_is_an_internally_tagged_enum() {
        assert_eq!(
            serde_json::to_value(JsError::OpsExceeded { ops: 7 }).unwrap(),
            serde_json::json!({ "kind": "ops_exceeded", "ops": 7 })
        );
        assert_eq!(
            serde_json::to_value(JsError::Cancelled).unwrap(),
            serde_json::json!({ "kind": "cancelled" })
        );
        assert_eq!(
            serde_json::to_value(JsError::Syntax {
                message: "boom".into(),
                line: Some(2),
                col: None
            })
            .unwrap(),
            serde_json::json!({ "kind": "syntax", "message": "boom", "line": 2, "col": null })
        );
    }

    #[test]
    fn config_rejects_a_zero_cap() {
        let mut c = caps();
        c.ops = 0;
        assert!(JsPlugin::validate(&JsConfig { default_caps: c }).is_err());
        assert!(JsPlugin::validate(&JsConfig {
            default_caps: caps()
        })
        .is_ok());
    }
}
