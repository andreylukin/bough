//! §0.2 runtime invariant for the projection seam:
//!
//! **`model_visible_is_ledgered`** — every [`SectionCites`] entry of every projection assembled
//! this session names a step or rollup id that EXISTS in the ledger.
//!
//! §3 lists model-visible ⟺ ledgered among the LEDGER invariants; it is implemented here because
//! the ledger Definition cannot see a projection section without depending on `projection`, which
//! would invert the seam (P1-D22, §0.2: consumers depend on Definitions, never the reverse). The
//! rule is §3's, unchanged; only its home moves. The check reads the ledger through the injected
//! handle, so it holds wherever the provider is mounted.
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use std::collections::BTreeSet;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{HashScope, Ledger, RollupId, StepId};
use parking_lot::Mutex;

use crate::section::{SectionCites, SectionId};

/// One assembled section's citation record, as observed.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub section: SectionId,
    pub cites: SectionCites,
}

/// What the assembler recorded this session, in assembly order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one assembled section. Called by the assembler at the end of `assemble`.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Everything recorded so far.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Drop the record. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// Forget everything recorded for `fiber`; a RELOAD keeps the `FiberUid`.
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber != fiber);
}

/// The spec the assembler's `Plugin::invariants()` returns.
pub fn model_visible_is_ledgered(plugin: &'static str) -> InvariantSpec {
    InvariantSpec {
        name: NAME,
        plugin,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

/// The invariant's name, in one place: the spec, the violation and the tests all read it here.
pub const NAME: &str = "model_visible_is_ledgered";

/// Read the ledger's row ids and judge the recorded stream against them.
///
/// `InvariantSpec::check` is a plain `fn`, so it cannot capture the `plugin` the spec was built
/// with; it reads the row's catalog name off the `Context` instead, exactly as the ledger's four
/// specs do, so the violation and the spec always name the same plugin.
async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let stream = seen();
    if stream.is_empty() {
        // Nothing was assembled this session. Vacuously true, and no ledger read at all.
        return Ok(());
    }
    let ledger = match ctx.get::<Ledger>() {
        Ok(l) => l,
        Err(e) => {
            return Err(_violation(
                &ctx,
                ctx.plugin_name(),
                format!("cannot check model-visible ⟺ ledgered: no ledger bound ({e})"),
            ))
        }
    };
    let mut steps = Vec::new();
    let mut rollups = Vec::new();
    for scope in [HashScope::Steps, HashScope::Rollups] {
        let rows = ledger.0.row_hashes(scope).await.map_err(|e| {
            _violation(
                &ctx,
                ctx.plugin_name(),
                format!("cannot check model-visible ⟺ ledgered: {e}"),
            )
        })?;
        for row in rows {
            match scope {
                HashScope::Steps => steps.push(StepId::new(row.id)),
                _ => rollups.push(RollupId::new(row.id)),
            }
        }
    }
    evaluate(&stream, &steps, &rollups)
        .map_err(|detail| _violation(&ctx, ctx.plugin_name(), detail))
}

/// The rule as a pure function: every cited id must appear in the ids the ledger holds.
pub fn evaluate(
    stream: &[Obs],
    known_steps: &[StepId],
    known_rollups: &[RollupId],
) -> Result<(), String> {
    let steps: BTreeSet<&StepId> = known_steps.iter().collect();
    let rollups: BTreeSet<&RollupId> = known_rollups.iter().collect();
    for obs in stream {
        for id in &obs.cites.steps {
            if !steps.contains(id) {
                return Err(format!(
                    "section `{}` is model-visible and cites step `{}`, which is not in the \
                     ledger; model-visible ⟺ ledgered",
                    obs.section, id
                ));
            }
        }
        for id in &obs.cites.rollups {
            if !rollups.contains(id) {
                return Err(format!(
                    "section `{}` is model-visible and cites rollup `{}`, which is not in the \
                     ledger; model-visible ⟺ ledgered",
                    obs.section, id
                ));
            }
        }
    }
    Ok(())
}

fn _violation(ctx: &Context, plugin: &'static str, detail: String) -> InvariantViolation {
    InvariantViolation {
        invariant: NAME,
        plugin,
        entry: ctx.entry_id().clone(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{RollupId, StepId};

    fn obs(section: &str, steps: &[&str], rollups: &[&str]) -> Obs {
        Obs {
            fiber: FiberUid(1),
            section: SectionId::new(section),
            cites: SectionCites {
                steps: steps.iter().map(StepId::new).collect(),
                rollups: rollups.iter().map(RollupId::new).collect(),
            },
        }
    }

    #[test]
    fn a_section_citing_a_missing_step_is_a_violation() {
        let known = [StepId::new("s1")];
        let detail = evaluate(&[obs("tail", &["s1", "s404"], &[])], &known, &[])
            .expect_err("a cited step that is not in the ledger must be reported");
        assert!(detail.contains("s404"), "unhelpful detail: {detail}");
        assert!(
            detail.contains("tail"),
            "the section must be named: {detail}"
        );
        assert!(
            detail.contains("model-visible"),
            "the detail must state the rule: {detail}"
        );
    }

    #[test]
    fn a_section_citing_a_missing_rollup_is_a_violation() {
        let steps = [StepId::new("s1")];
        let rollups = [RollupId::new("r1")];
        let detail = evaluate(&[obs("digest", &["s1"], &["r9"])], &steps, &rollups)
            .expect_err("a cited rollup that is not in the ledger must be reported");
        assert!(detail.contains("r9"), "unhelpful detail: {detail}");
        assert!(detail.contains("digest"));
    }

    #[test]
    fn a_fully_cited_projection_reports_nothing() {
        let steps = [StepId::new("s1"), StepId::new("s2")];
        let rollups = [RollupId::new("r1")];
        let stream = vec![
            obs("identity", &[], &[]),
            obs("digest", &[], &["r1"]),
            obs("tail", &["s1", "s2"], &[]),
        ];
        assert_eq!(evaluate(&stream, &steps, &rollups), Ok(()));
        // An empty session is vacuously clean, and so is a ledger with rows nobody cited.
        assert_eq!(evaluate(&[], &steps, &rollups), Ok(()));
    }

    #[test]
    fn forgetting_a_fiber_drops_only_its_records() {
        clear();
        record(Obs {
            fiber: FiberUid(1),
            ..obs("a", &[], &[])
        });
        record(Obs {
            fiber: FiberUid(2),
            ..obs("b", &[], &[])
        });
        forget(FiberUid(1));
        let left = seen();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].section.as_str(), "b");
        clear();
    }
}
