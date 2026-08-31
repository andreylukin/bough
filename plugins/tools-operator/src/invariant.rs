//! §0.2 runtime invariant for `bough-plugin-tools-operator`:
//!
//! **Every `schedule/fired` names a `schedule/intent` that exists and was not already fired.**
//! A double fire is a duplicated wake and, through it, duplicated outward work; a fire with no
//! intent is a wake nobody asked for. WP-4 owns the recorder and the wiring.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

/// What the row observed about scheduled intents this session.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    /// Every `schedule/intent` id appended, in order.
    pub intents: Vec<String>,
    /// Every `schedule/fired` id appended, in order.
    pub fired: Vec<String>,
}

static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// Record one observation window.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Record the intents the watcher can see. Idempotent: the fold re-reads the same rows every
/// tick, so a repeat is not news.
pub fn note_intents(fiber: FiberUid, intents: Vec<String>) {
    with(fiber, |obs| {
        for i in intents {
            if !obs.intents.contains(&i) {
                obs.intents.push(i);
            }
        }
    });
}

/// Record one fire this process actually performed. NOT idempotent, on purpose: the whole point
/// of the check is that a second append for one id is visible here as a second entry.
pub fn note_fire(fiber: FiberUid, id: String) {
    with(fiber, |obs| obs.fired.push(id));
}

fn with(fiber: FiberUid, f: impl FnOnce(&mut Obs)) {
    let mut seen = SEEN.lock();
    if !seen.iter().any(|o| o.fiber == fiber) {
        seen.push(Obs {
            fiber,
            intents: Vec::new(),
            fired: Vec::new(),
        });
    }
    let obs = seen
        .iter_mut()
        .find(|o| o.fiber == fiber)
        .expect("just ensured");
    f(obs);
}

/// Forget everything recorded for `fiber` (registered as an inverse by `apply`).
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Drop the record. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// The whole invariant as a pure function of the observed windows.
pub fn evaluate(windows: &[Obs]) -> Result<(), String> {
    for w in windows {
        for id in &w.fired {
            if !w.intents.contains(id) {
                return Err(format!(
                    "schedule/fired names `{id}`, for which no schedule/intent exists; a wake \
                     nobody asked for"
                ));
            }
            let n = w.fired.iter().filter(|f| *f == id).count();
            if n > 1 {
                return Err(format!(
                    "the intent `{id}` fired {n} times; a scheduled intent fires exactly once"
                ));
            }
        }
    }
    Ok(())
}

/// The spec `OperatorPlugin::invariants` returns.
pub fn every_fire_names_a_live_intent() -> InvariantSpec {
    InvariantSpec {
        name: "every_schedule_fired_names_a_live_intent_and_fires_once",
        plugin: PLUGIN,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

const PLUGIN: &str = crate::PLUGIN_NAME;

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "every_schedule_fired_names_a_live_intent_and_fires_once",
        plugin: PLUGIN,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(intents: &[&str], fired: &[&str]) -> Obs {
        Obs {
            fiber: FiberUid(1),
            intents: intents.iter().map(|s| s.to_string()).collect(),
            fired: fired.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn one_fire_per_intent_is_clean() {
        assert_eq!(evaluate(&[obs(&["a", "b"], &["a"])]), Ok(()));
        assert_eq!(evaluate(&[]), Ok(()), "an idle session is vacuously clean");
    }

    #[test]
    fn a_fire_with_no_intent_is_a_violation() {
        let d = evaluate(&[obs(&["a"], &["z"])]).expect_err("an orphan fire must be reported");
        assert!(d.contains("no schedule/intent"), "{d}");
    }

    #[test]
    fn a_double_fire_is_a_violation() {
        let d = evaluate(&[obs(&["a"], &["a", "a"])]).expect_err("a double fire must be reported");
        assert!(d.contains("fired 2 times"), "{d}");
    }
}
