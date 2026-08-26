//! Invariant: `hello` is a TEST INSTRUMENT, not a product row. It exists to prove the kernel
//! (§17 Phase 0) and every interesting moment pushes a line onto a shared trace the tests assert
//! on in order. It is deleted or demoted at Phase 8.
//!
//! Three catalog names live here (Decision D14, a deliberate exception to one-crate-one-row):
//! `hello` (the consumer, `inject: ["greeting"]`), `greeting-echo` and `greeting-shout` (two
//! providers of the same key whose greeting output is EQUAL — which is what makes V2 meaningful:
//! hello reloads because the provider fiber changed, not because the value did).

pub mod invariant;
pub mod provider;

use std::sync::Arc;

use bough_kernel::{
    Context, Inject, InvariantSpec, KernelError, Plugin, PluginError, Reconfigure, ServiceKey,
};

pub use provider::{EchoProvider, ProviderConfig, ShoutProvider};

/// The Service Definition: the one key Phase 0 defines anywhere.
pub struct Greeting;

impl ServiceKey for Greeting {
    type Value = GreetingHandle;
    const NAME: &'static str = "greeting";
}

/// The concrete handle newtype the key's value is (Decision D5: `ServiceKey::Value` is `Sized`,
/// so a trait-object service is wrapped by the Service Definition that owns it).
#[derive(Clone)]
pub struct GreetingHandle(pub Arc<dyn GreetingSink>);

/// What a greeting provider does.
pub trait GreetingSink: Send + Sync + 'static {
    fn greet(&self, who: &str) -> String;
    /// The catalog name of the plugin behind this binding; the swap test asserts on it.
    fn provider(&self) -> &'static str;
}

/// A key `hello` never declares in `inject:`, used by exactly one test hook (Decision D16).
///
/// Nothing provides it and nothing ever will: its whole purpose is that reading it is the §0.3
/// capability failure, reported at the point of use and naming the key and the plugin (V8).
pub struct LedgerProbe;

impl ServiceKey for LedgerProbe {
    type Value = ();
    const NAME: &'static str = "ledger";
}

/// How chatty `hello` is. Immaterial: a change to this field is absorbed by `reconfigure` without
/// a reload, which is what proves "config is handed to the plugin, which reloads only on a
/// material diff" (§0.3).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
}

/// The consumer row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HelloConfig {
    /// Material: a change reloads the fiber.
    pub who: String,
    /// Immaterial: absorbed by `reconfigure`.
    #[serde(default)]
    pub log_level: LogLevel,
    /// Test hook (Decision D16): makes the invariant fail on purpose. V9's vehicle.
    #[serde(default)]
    pub plant_violation: bool,
    /// Test hook (Decision D16): `apply` reads a key it never declared. V8's vehicle. The only
    /// accepted value is `"ledger"` — [`LedgerProbe`] is the key that gets read; `validate`
    /// rejects anything else, purely and synchronously (§0.5).
    #[serde(default)]
    pub read_undeclared: Option<String>,
    /// Test hook: mount a nested child row from `apply`, so unloading this fiber has something to
    /// cascade to (V3). Deviation from the plan's four fields, noted in `docs/phase-0-plan.md`'s
    /// terms: `ctx.mount` has no other Phase-0 caller and V3 names a hello-side test for it.
    #[serde(default)]
    pub mount_child: bool,
}

/// The consumer.
pub struct HelloPlugin;

#[async_trait::async_trait]
impl Plugin for HelloPlugin {
    const NAME: &'static str = "hello";
    type Config = HelloConfig;

    fn inject() -> Inject {
        Inject::required(["greeting"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        match cfg.read_undeclared.as_deref() {
            None | Some("ledger") => Ok(()),
            Some(other) => Err(bough_kernel::ConfigError::Rejected {
                detail: format!(
                    "read_undeclared: the only probe key this fixture knows is `ledger`, not `{other}`"
                ),
            }),
        }
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let t = trace::global();
        t.push(Self::NAME, "apply");
        let entry = ctx.entry_id().clone();

        // V8: read a key this row never declared. The failure is the point.
        if cfg.read_undeclared.is_some() {
            let err = match ctx.get::<LedgerProbe>() {
                Ok(_) => KernelError::UndeclaredService {
                    plugin: Self::NAME,
                    entry: entry.clone(),
                    key: LedgerProbe::NAME,
                },
                Err(e) => e,
            };
            trace::record_error(err.to_string());
            t.push(Self::NAME, "undeclared");
            return Err(PluginError::new(entry, anyhow::Error::new(err)));
        }

        let greeting = ctx
            .get::<Greeting>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // Which provider fiber this activation bound against. The swap test reads this line.
        t.push(Self::NAME, greeting.0.provider());

        // Registered FIRST, so it unwinds LAST: `hello:unload` closes this fiber's teardown.
        let marker = t.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || marker.push(HelloPlugin::NAME, "unload"));
            Ok(())
        })
        .await?;

        // Three numbered effects, so `hello_effects_unwind_lifo_on_unload` has an order to see.
        for moment in ["effect-1", "effect-2", "effect-3"] {
            let t = t.clone();
            ctx.effect(move |e| async move {
                e.defer_sync(move || t.push(HelloPlugin::NAME, moment));
                Ok(())
            })
            .await?;
        }

        // The stream `src/invariant.rs` polices, recorded by its listener.
        ctx.on_parallel::<GreetedEvent, _, _>(move |g| async move {
            invariant::record(g.fiber, g.seq);
        })
        .await?;

        if cfg.mount_child {
            // A nested mount is an effect of this fiber, so unloading hello cascades to it. The
            // child provides `greeting` in its own realm so it cannot disturb the root binding.
            let child = format!(
                "id: {}.child\nplugin: greeting-shout\nconfig: {{ suffix: \"\" }}\nisolate: {{ greeting: hello-child }}\n",
                entry.as_str()
            );
            let child: bough_kernel::Entry = serde_yaml::from_str(&child)
                .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::new(e)))?;
            ctx.mount(child)
                .await
                .map_err(|e| PluginError::new(entry.clone(), e))?;
        }

        // `fiber_uid()`, not `fiber().uid()`: the fixture needs the identity, not a handle, and
        // resolving a handle would drag the whole fiber runtime into a plugin body for nothing.
        let uid = ctx.fiber_uid();
        let text = greeting.0.greet(&cfg.who);
        ctx.parallel::<GreetedEvent>(Greeted {
            fiber: uid,
            seq: 1,
            text: text.clone(),
        })
        .await;
        // `plant_violation` repeats the seq instead of advancing it: the violation V9 detects.
        let second = if cfg.plant_violation { 1 } else { 2 };
        ctx.parallel::<GreetedEvent>(Greeted {
            fiber: uid,
            seq: second,
            text,
        })
        .await;

        Ok(())
    }

    /// `Applied` when only `log_level` differs; `Reload` otherwise.
    fn reconfigure(_ctx: &Context, old: &Self::Config, new: &Self::Config) -> Reconfigure {
        let mut probe = old.clone();
        probe.log_level = new.log_level;
        if probe == *new {
            Reconfigure::Applied
        } else {
            Reconfigure::Reload
        }
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::greeted_seq_is_monotonic()]
    }
}

/// A greeting was emitted. The stream `plugins/hello/src/invariant.rs` polices.
#[derive(Clone, Debug)]
pub struct Greeted {
    pub fiber: bough_kernel::FiberUid,
    pub seq: u64,
    pub text: String,
}

/// `hello/greeted`.
///
/// Dispatched PARALLEL rather than EMIT: the invariant runner reads what this event's listener
/// recorded, and a fire-and-forget dispatch would make "has the listener run yet?" a race in
/// every V9 run. The four dispatch modes are proven by `event::tests`, not by the fixture.
pub struct GreetedEvent;

impl bough_kernel::ParallelEvent for GreetedEvent {
    const NAME: &'static str = "hello/greeted";
    type Payload = Greeted;
}

/// The observation channel the Phase 0 tests assert on, in order: `("hello", "apply")`,
/// `("greeting-echo", "unload")`, and so on.
pub mod trace {
    use parking_lot::{Mutex, MutexGuard};
    use std::sync::{Arc, OnceLock};

    /// A shared, ordered log of `(plugin, moment)` pairs.
    #[derive(Clone, Default)]
    pub struct Trace(Arc<Mutex<Vec<(&'static str, &'static str)>>>);

    impl Trace {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn push(&self, plugin: &'static str, moment: &'static str) {
            self.0.lock().push((plugin, moment));
        }
        pub fn lines(&self) -> Vec<(&'static str, &'static str)> {
            self.0.lock().clone()
        }
        pub fn clear(&self) {
            self.0.lock().clear();
        }
        /// Index of the first line equal to `line`, for the "strictly before" assertions.
        pub fn position(&self, line: (&'static str, &'static str)) -> Option<usize> {
            self.0.lock().iter().position(|l| *l == line)
        }
    }

    static TRACE: OnceLock<Trace> = OnceLock::new();
    static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// The process-wide trace the fixture writes to.
    pub fn global() -> Trace {
        TRACE.get_or_init(Trace::new).clone()
    }

    /// Record an error the fixture raised on purpose, so a test can assert on its text without
    /// needing a handle to the failed fiber.
    pub fn record_error(detail: String) {
        ERRORS.lock().push(detail);
    }

    /// Errors recorded so far, oldest first.
    pub fn errors() -> Vec<String> {
        ERRORS.lock().clone()
    }

    /// Serialise the tests that read this process-wide state, and start each of them from a
    /// cleared trace. Held for the test's whole body.
    pub fn test_lock() -> TestGuard {
        let guard = TEST_LOCK.lock();
        global().clear();
        ERRORS.lock().clear();
        crate::invariant::clear();
        TestGuard(guard)
    }

    /// The guard [`test_lock`] returns. Holding it IS its purpose.
    #[allow(dead_code)]
    pub struct TestGuard(MutexGuard<'static, ()>);
}

bough_kernel::register_plugin!(HelloPlugin);
bough_kernel::register_plugin!(EchoProvider);
bough_kernel::register_plugin!(ShoutProvider);
