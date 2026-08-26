//! Invariant (§0.5): validation is PURE and SYNCHRONOUS. A window that cannot hold `min_samples`
//! would flag every agent forever, so it is refused at boot rather than explained at runtime.

use bough_kernel::ConfigError;
use bough_plugin_ledger::{Seq, SeqRange};

use crate::DriftConfig;

/// Refuse: `window_steps == 0`; `min_samples > window_steps`; a `thought_len_cv_flag` that is not
/// finite and positive; a `tool_entropy_flag` outside `0.0..=1.0`.
pub fn validate(cfg: &DriftConfig) -> Result<(), ConfigError> {
    let reject = |detail: String| Err(ConfigError::Rejected { detail });
    if cfg.window_steps == 0 {
        return reject("window_steps must be > 0".to_string());
    }
    if cfg.min_samples > cfg.window_steps {
        return reject(format!(
            "min_samples ({}) exceeds window_steps ({}): every agent would be flagged \
             `too_few_samples` forever",
            cfg.min_samples, cfg.window_steps
        ));
    }
    if !cfg.thought_len_cv_flag.is_finite() || cfg.thought_len_cv_flag <= 0.0 {
        return reject(format!(
            "thought_len_cv_flag must be finite and > 0, got {}",
            cfg.thought_len_cv_flag
        ));
    }
    if !cfg.tool_entropy_flag.is_finite()
        || cfg.tool_entropy_flag < 0.0
        || cfg.tool_entropy_flag > 1.0
    {
        return reject(format!(
            "tool_entropy_flag is a NORMALISED entropy and must lie in 0.0..=1.0, got {}",
            cfg.tool_entropy_flag
        ));
    }
    if cfg.max_evidence_cites == 0 {
        return reject(
            "max_evidence_cites must be > 0: the rebuilt about-line is EVIDENCE, and the ledger \
             refuses an evidence step with no cites"
                .to_string(),
        );
    }
    if cfg.max_state_chars == 0 {
        return reject(
            "max_state_chars must be > 0: a state half of zero characters is not a \
                       rebuild"
                .to_string(),
        );
    }
    Ok(())
}

/// The seq window the signals are computed over: the last `window_steps` seqs below `head`.
///
/// The explicit `resolve(request) -> Spec` step (§0.2): the clamp at seq 1 lives here, not as a
/// `?? default` inside the read path. A trajectory with no head has an EMPTY window, spelled
/// `1..=0`, which every query reads as "no rows" rather than "everything".
pub fn window(head: Option<Seq>, cfg: &DriftConfig) -> SeqRange {
    match head {
        Some(head) if head.0 >= 1 => SeqRange {
            from: Seq(head.0.saturating_sub(cfg.window_steps as u64 - 1).max(1)),
            to: head,
        },
        _ => SeqRange {
            from: Seq(1),
            to: Seq(0),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DriftConfig {
        DriftConfig {
            window_steps: 500,
            min_samples: 20,
            thought_len_cv_flag: 1.2,
            tool_entropy_flag: 0.35,
            max_evidence_cites: 24,
            max_state_chars: 400,
        }
    }

    #[test]
    fn a_sane_row_validates() {
        validate(&cfg()).expect("the shipped row is valid");
    }

    #[test]
    fn the_reset_bounds_are_config_and_a_zero_is_refused() {
        for mutate in [
            (|c: &mut DriftConfig| c.max_evidence_cites = 0) as fn(&mut DriftConfig),
            |c| c.max_state_chars = 0,
        ] {
            let mut c = cfg();
            mutate(&mut c);
            validate(&c).expect_err("a zero reset bound must fail the boot");
        }
    }

    #[test]
    fn a_window_that_cannot_hold_min_samples_is_refused() {
        let mut c = cfg();
        c.window_steps = 10;
        c.min_samples = 11;
        let err = validate(&c).expect_err("min_samples > window_steps must be refused");
        assert!(err.to_string().contains("min_samples"), "{err}");

        c.window_steps = 0;
        assert!(validate(&c).is_err());
    }

    #[test]
    fn the_two_thresholds_are_range_checked() {
        let mut c = cfg();
        c.thought_len_cv_flag = 0.0;
        assert!(validate(&c).is_err());
        c.thought_len_cv_flag = f64::NAN;
        assert!(validate(&c).is_err());

        let mut c = cfg();
        c.tool_entropy_flag = 1.5;
        let err = validate(&c).expect_err("a normalised entropy above 1.0 is not a threshold");
        assert!(err.to_string().contains("NORMALISED"), "{err}");
        c.tool_entropy_flag = -0.1;
        assert!(validate(&c).is_err());
        // The two ends of the legal range are legal.
        c.tool_entropy_flag = 0.0;
        assert!(validate(&c).is_ok());
        c.tool_entropy_flag = 1.0;
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn the_window_clamps_at_seq_one_and_is_empty_without_a_head() {
        let mut c = cfg();
        c.window_steps = 10;
        assert_eq!(
            window(Some(Seq(100)), &c),
            SeqRange {
                from: Seq(91),
                to: Seq(100)
            }
        );
        assert_eq!(
            window(Some(Seq(3)), &c),
            SeqRange {
                from: Seq(1),
                to: Seq(3)
            }
        );
        let empty = window(None, &c);
        assert!(empty.from > empty.to, "no head ⇒ an empty window");
    }
}
