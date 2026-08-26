//! Invariant (§0.5): validation is PURE and SYNCHRONOUS, and misconfiguration fails loud. A
//! config listing a kind in [`bough_plugin_rollups::NEVER_EXPIRABLE`] is refused at boot rather
//! than silently intersected away at runtime — the runtime intersection stays as the second lock.

use bough_kernel::ConfigError;

use crate::ReconConfig;

/// Refuse: `batch_steps == 0`; `max_calls_per_pass == 0`; `stale_after_days <= 0`; an
/// `expirable_kinds` entry that is never expirable (V7).
pub fn validate(cfg: &ReconConfig) -> Result<(), ConfigError> {
    let reject = |detail: String| Err(ConfigError::Rejected { detail });
    if cfg.batch_steps == 0 {
        return reject(
            "`batch_steps` must be at least 1: a pass over no steps is not a pass".into(),
        );
    }
    if cfg.max_calls_per_pass == 0 {
        return reject(
            "`max_calls_per_pass` must be at least 1: a pass that may make no call can judge \
             no contradiction"
                .into(),
        );
    }
    if cfg.stale_after_days <= 0 {
        return reject(
            "`stale_after_days` must be positive: a non-positive threshold expires evidence the \
             moment it is written"
                .into(),
        );
    }
    if cfg.max_contradiction_pairs == 0 {
        return reject("`max_contradiction_pairs` must be at least 1".into());
    }
    if cfg.distill_max_tokens <= 0 {
        return reject("`distill_max_tokens` must be positive".into());
    }
    for kind in &cfg.expirable_kinds {
        if bough_plugin_rollups::NEVER_EXPIRABLE.contains(&kind.as_str()) {
            return reject(format!(
                "`expirable_kinds` names `{kind}`, which is never expirable (§3): a pin's only \
                 relief valve is supersession"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ReconConfig {
        ReconConfig {
            batch_steps: 400,
            stale_after_days: 90,
            expirable_kinds: vec!["mail/delivered".into(), "tool/result".into()],
            max_contradiction_pairs: 24,
            max_calls_per_pass: 6,
            distill_max_tokens: 2048,
        }
    }

    #[test]
    fn the_bundle_row_validates() {
        validate(&cfg()).expect("the `bough-base` row is a legal config");
    }

    #[test]
    fn a_never_expirable_kind_is_refused_at_boot() {
        let mut c = cfg();
        c.expirable_kinds.push("pin/set".into());
        let err = validate(&c).expect_err("a pin kind must fail the boot");
        assert!(format!("{err}").contains("pin/set"), "{err}");
    }

    #[test]
    fn the_degenerate_numbers_are_refused() {
        for mutate in [
            (|c: &mut ReconConfig| c.batch_steps = 0) as fn(&mut ReconConfig),
            |c| c.max_calls_per_pass = 0,
            |c| c.stale_after_days = 0,
            |c| c.stale_after_days = -1,
            |c| c.max_contradiction_pairs = 0,
            |c| c.distill_max_tokens = 0,
        ] {
            let mut c = cfg();
            mutate(&mut c);
            validate(&c).expect_err("a degenerate number must fail the boot");
        }
    }
}
