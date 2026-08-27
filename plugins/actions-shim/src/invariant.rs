//! §0.2 runtime invariant for `bough-plugin-actions-shim`:
//!
//! **One `gh` invocation per `action/intent` idem key, over the process's whole life.** This is
//! §7's "never re-executed" fact, checked continuously rather than only by V3's crash test: an
//! idem key that was acted on twice is the exact failure a journal + marker exists to prevent.

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::IdemKey;
use parking_lot::Mutex;

/// The invariant's name, as a violation reports it.
pub const NAME: &str = "one_gh_invocation_per_idem_key";

/// Process-global, because the fact is about the PROCESS: two rows of this Provider mounted at
/// once must still not act twice on one key.
static INVOCATIONS: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());

/// Record one invocation. Called from `execute`, immediately before the outward act.
pub fn record(idem: &IdemKey) {
    *INVOCATIONS
        .lock()
        .entry(idem.as_str().to_string())
        .or_insert(0) += 1;
}

/// Every idem key this process invoked, with its count.
pub fn invocations() -> Vec<(IdemKey, u32)> {
    INVOCATIONS
        .lock()
        .iter()
        .map(|(k, n)| (IdemKey::new(k.clone()), *n))
        .collect()
}

/// Forget the record. The row's disposal path, and a test's setup.
pub fn forget() {
    INVOCATIONS.lock().clear();
}

/// PURE: the check — no idem key was invoked twice.
pub fn check_counts(counts: &[(IdemKey, u32)]) -> Result<(), String> {
    for (key, n) in counts {
        if *n > 1 {
            return Err(format!(
                "idem key `{}` was acted on {n} times; an act on the world is performed at most once",
                key.as_str()
            ));
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    check_counts(&invocations()).map_err(|detail| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_invocation_per_key_is_the_whole_check() {
        let a = IdemKey::new("aaa");
        let b = IdemKey::new("bbb");
        assert!(check_counts(&[(a.clone(), 1), (b.clone(), 1)]).is_ok());
        let err = check_counts(&[(a, 1), (b.clone(), 2)]).expect_err("twice is a violation");
        assert!(
            err.contains(b.as_str()),
            "the violation names the key; got {err}"
        );
    }
}
