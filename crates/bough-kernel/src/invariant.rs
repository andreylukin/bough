//! Invariant: the invariant runner REPORTS and never acts. It collects the specs of every ACTIVE
//! fiber's plugin, runs them at their cadence, records violations and emits
//! `kernel/invariant-violated`. It never panics and never unloads anybody — a violation is a
//! report, so a false positive can never take the tree down (§0.2).
//!
//! It exists only when `KernelOptions::invariants` is true: the `dev` profile and the test
//! harness. In `tui` and `headless` it is not created at all.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;

use crate::context::Context;
use crate::fiber::EntryId;
use crate::kernel::KernelEvents;

/// One invariant a plugin crate owns, declared from its `src/invariant.rs`.
pub struct InvariantSpec {
    pub name: &'static str,
    pub plugin: &'static str,
    pub cadence: Cadence,
    pub check: fn(Context) -> BoxFuture<'static, Result<(), InvariantViolation>>,
}

/// When an invariant runs.
///
/// Phase 0 dispatches [`Cadence::OnQuiesce`] only. The other two are declared because §0.2 asks for
/// invariants that hold OVER TIME and later phases need them; until then, collecting one is
/// reported at WARN and counted by [`InvariantRunner::unsupported`], so a plugin that declares one
/// is never silently unchecked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cadence {
    /// Once, each time the tree quiesces.
    OnQuiesce,
    /// On a timer. NOT DISPATCHED in Phase 0.
    Interval(Duration),
    /// Whenever the named event dispatches. NOT DISPATCHED in Phase 0.
    OnEvent(&'static str),
}

impl Cadence {
    /// Whether the runner actually dispatches this cadence today.
    pub fn is_dispatched(&self) -> bool {
        matches!(self, Cadence::OnQuiesce)
    }
}

/// A violation, as reported. Carries enough to act on without reading the check's source.
#[derive(Clone, Debug)]
pub struct InvariantViolation {
    pub invariant: &'static str,
    pub plugin: &'static str,
    pub entry: EntryId,
    pub detail: String,
}

/// How a spec is actually invoked.
///
/// A check takes the row's `Context`, which only WP-2's `KernelCore` can mint; the runner is
/// written against this seam so its own behaviour — collect, run, record, never unload — is
/// testable without one.
pub trait CheckHost: Send + Sync + 'static {
    fn run(
        &self,
        entry: &EntryId,
        spec: &InvariantSpec,
    ) -> BoxFuture<'static, Result<(), InvariantViolation>>;
}

/// The runner. Created by [`crate::Kernel`] iff `KernelOptions::invariants`.
pub struct InvariantRunner {
    events: Arc<dyn KernelEvents>,
    host: Arc<dyn CheckHost>,
    specs: Mutex<Vec<(EntryId, InvariantSpec)>>,
    violations: Mutex<Vec<InvariantViolation>>,
    /// Specs whose cadence this runner does not dispatch, most recent collection only.
    unsupported: Mutex<Vec<(EntryId, &'static str)>>,
}

impl InvariantRunner {
    /// A runner over a check host.
    ///
    /// DEVIATION from plan §2.9, which spells this `start(ctx, specs)`: a check runs against the
    /// ROW's context, not one context for the whole runner, and only the kernel can map a row to
    /// its context. The mapping is the `CheckHost`; `Kernel::start_invariants` supplies it.
    pub fn with_host(events: Arc<dyn KernelEvents>, host: Arc<dyn CheckHost>) -> Self {
        InvariantRunner {
            events,
            host,
            specs: Mutex::new(Vec::new()),
            violations: Mutex::new(Vec::new()),
            unsupported: Mutex::new(Vec::new()),
        }
    }

    /// Replace the spec set. Called after every reconciliation, so a plugin that just unloaded
    /// stops being checked.
    pub fn collect_specs(&self, specs: Vec<(EntryId, InvariantSpec)>) {
        let mut unsupported = Vec::new();
        for (id, s) in &specs {
            if !s.cadence.is_dispatched() {
                tracing::warn!(
                    entry = %id,
                    plugin = s.plugin,
                    invariant = s.name,
                    cadence = ?s.cadence,
                    "invariant cadence is not dispatched in this phase; the check will NOT run"
                );
                unsupported.push((id.clone(), s.name));
            }
        }
        *self.unsupported.lock() = unsupported;
        *self.specs.lock() = specs;
    }

    /// Collected specs whose cadence the runner does not dispatch. Never silent: `collect_specs`
    /// also logs each one at WARN.
    pub fn unsupported(&self) -> Vec<(EntryId, &'static str)> {
        self.unsupported.lock().clone()
    }

    pub fn spec_count(&self) -> usize {
        self.specs.lock().len()
    }

    /// Every violation recorded so far.
    pub fn violations(&self) -> Vec<InvariantViolation> {
        self.violations.lock().clone()
    }

    /// Run every `OnQuiesce` spec once. Called by the kernel after reconciliation settles.
    pub async fn run_on_quiesce(&self) {
        let futures: Vec<_> = {
            let specs = self.specs.lock();
            specs
                .iter()
                .filter(|(_, s)| matches!(s.cadence, Cadence::OnQuiesce))
                .map(|(id, s)| self.host.run(id, s))
                .collect()
        };
        for f in futures {
            // A check that returns Err is a report. There is no branch here that unloads anything,
            // and there must never be one (§0.2).
            if let Err(v) = f.await {
                let v = Arc::new(v);
                self.violations.lock().push((*v).clone());
                self.events.invariant_violated(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fiber::FiberState;
    use crate::kernel::tests::{row, RecordingEvents, TreeHarness};
    use crate::kernel::{Kernel, KernelOptions};

    fn spec() -> InvariantSpec {
        InvariantSpec {
            name: "greeted-seq-is-monotonic",
            plugin: "hello",
            cadence: Cadence::OnQuiesce,
            check: |_ctx| Box::pin(async { Ok(()) }),
        }
    }

    /// A host that plants one violation, so the runner's own behaviour is what is under test.
    struct PlantedHost {
        fail: bool,
    }

    impl CheckHost for PlantedHost {
        fn run(
            &self,
            entry: &EntryId,
            spec: &InvariantSpec,
        ) -> BoxFuture<'static, Result<(), InvariantViolation>> {
            let v = InvariantViolation {
                invariant: spec.name,
                plugin: spec.plugin,
                entry: entry.clone(),
                detail: "seq 7 repeated after seq 7".to_string(),
            };
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    Err(v)
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn runner_reports_a_planted_violation() {
        let events = Arc::new(RecordingEvents::default());
        let runner = InvariantRunner::with_host(events, Arc::new(PlantedHost { fail: true }));
        runner.collect_specs(vec![(EntryId::new("hello.greeter"), spec())]);
        runner.run_on_quiesce().await;

        let v = runner.violations();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].invariant, "greeted-seq-is-monotonic");
        assert_eq!(v[0].entry.as_str(), "hello.greeter");
        assert!(v[0].detail.contains("seq 7"));
    }

    /// §0.2 asks for invariants that hold OVER TIME. Phase 0 dispatches `OnQuiesce` only; a spec
    /// with any other cadence must be reported, never silently unchecked. This test pins the
    /// silence that used to be there — delete it when the cadences are implemented.
    #[tokio::test]
    async fn an_undispatched_cadence_is_reported_and_does_not_run() {
        let events = Arc::new(RecordingEvents::default());
        let runner = InvariantRunner::with_host(events, Arc::new(PlantedHost { fail: true }));
        let timed = InvariantSpec {
            name: "over-time",
            cadence: Cadence::Interval(Duration::from_millis(1)),
            ..spec()
        };
        let on_event = InvariantSpec {
            name: "on-step",
            cadence: Cadence::OnEvent("ledger/appended"),
            ..spec()
        };
        runner.collect_specs(vec![
            (EntryId::new("a"), timed),
            (EntryId::new("b"), on_event),
            (EntryId::new("c"), spec()),
        ]);

        let unsupported = runner.unsupported();
        assert_eq!(
            unsupported.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            vec!["over-time", "on-step"],
            "an undispatched cadence must be named, not swallowed"
        );

        runner.run_on_quiesce().await;
        assert_eq!(
            runner.violations().len(),
            1,
            "only the OnQuiesce spec ran, which is exactly why the other two must be reported"
        );
    }

    #[tokio::test]
    async fn runner_is_inert_when_disabled() {
        // `invariants: false` is the tui/headless profile: the runner is not created at all, so a
        // check that would fail is never even collected.
        let h = TreeHarness::new();
        assert!(!h.kernel.options().invariants);
        h.apply(vec![row("a").plugin("one")]).await;
        h.kernel.start_invariants();
        assert!(h.kernel.violations().is_empty());

        // And with the flag on, the same kernel does create one.
        let on = Kernel::with_parts(
            None,
            KernelOptions {
                invariants: true,
                ..KernelOptions::default()
            },
            Arc::new(NoopFactory),
            Arc::new(NoopResolver),
            Arc::new(RecordingEvents::default()),
        );
        on.start_invariants();
        assert!(
            on.violations().is_empty(),
            "created, and reporting nothing yet"
        );
    }

    #[tokio::test]
    async fn a_violation_does_not_unload_the_plugin() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one")]).await;
        let uid = h.uid("a");

        let runner = InvariantRunner::with_host(
            Arc::new(RecordingEvents::default()),
            Arc::new(PlantedHost { fail: true }),
        );
        runner.collect_specs(vec![(EntryId::new("a"), spec())]);
        runner.run_on_quiesce().await;
        h.kernel.quiesce().await;

        assert_eq!(runner.violations().len(), 1);
        assert_eq!(
            h.state("a"),
            FiberState::Active,
            "a report is not an unload"
        );
        assert_eq!(h.uid("a"), uid);
        assert_eq!(h.trace.count("a/one:unwind"), 0);
    }

    struct NoopFactory;
    impl crate::kernel::BodyFactory for NoopFactory {
        fn build(
            &self,
            _entry: &crate::config::Entry,
        ) -> Result<Arc<dyn crate::fiber::FiberBody>, crate::error::ComposeError> {
            unreachable!("nothing is mounted in this test")
        }
        fn reconfigure(
            &self,
            _current: &Arc<dyn crate::fiber::FiberBody>,
            _old: &crate::config::Entry,
            _new: &crate::config::Entry,
        ) -> Result<
            (Arc<dyn crate::fiber::FiberBody>, crate::plugin::Reconfigure),
            crate::error::ComposeError,
        > {
            unreachable!("nothing is reconfigured in this test")
        }
        fn static_name(&self, _plugin: &str) -> Option<&'static str> {
            None
        }
    }

    struct NoopResolver;
    impl crate::fiber::Resolver for NoopResolver {
        fn resolve(
            &self,
            _key: &str,
            _realm: Option<&crate::config::RealmLabel>,
        ) -> Option<crate::service::ProviderUid> {
            None
        }
    }
}
