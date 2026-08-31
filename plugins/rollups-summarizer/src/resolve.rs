//! Invariant (§0.5): validation is PURE and SYNCHRONOUS, and a stamp that names a prompt the
//! binary does not have is a lie — so `prompt_ver` must resolve in [`crate::prompts`] or the row
//! refuses to boot. Misconfiguration fails loud.

use bough_kernel::ConfigError;
use bough_plugin_rollups::{TierCfg, WindowCfg};
use chrono::Duration;

use crate::SummarizerConfig;

fn rejected(detail: impl Into<String>) -> ConfigError {
    ConfigError::Rejected {
        detail: detail.into(),
    }
}

/// Refuse: an empty or unknown `prompt_ver`; `fanout < 2`; `max_tier == 0`;
/// `min_window_steps > max_window_steps`; `max_calls_per_pass == 0`; `gap_minutes == 0`.
pub fn validate(cfg: &SummarizerConfig) -> Result<(), ConfigError> {
    if cfg.prompt_ver.trim().is_empty() {
        return Err(rejected(
            "`prompt_ver` is empty: a sealed block must be stamped with the prompt that produced it",
        ));
    }
    if !crate::prompts::covers_every_phase(&cfg.prompt_ver) {
        return Err(rejected(format!(
            "`prompt_ver: {}` names a prompt this binary does not have; it ships {:?}",
            cfg.prompt_ver,
            crate::prompts::versions()
        )));
    }
    if cfg.fanout < 2 {
        return Err(rejected(format!(
            "`fanout: {}` is below 2: a tier that reduces one child is not a tier",
            cfg.fanout
        )));
    }
    if cfg.max_tier == 0 {
        return Err(rejected("`max_tier: 0` would build no tier at all"));
    }
    if cfg.min_window_steps > cfg.max_window_steps {
        return Err(rejected(format!(
            "`min_window_steps: {}` exceeds `max_window_steps: {}`: no window could ever be sealed",
            cfg.min_window_steps, cfg.max_window_steps
        )));
    }
    if cfg.max_window_steps == 0 {
        return Err(rejected("`max_window_steps: 0` would cut empty windows"));
    }
    if cfg.max_calls_per_pass == 0 {
        return Err(rejected(
            "`max_calls_per_pass: 0` would make every pass a no-op that still reports success",
        ));
    }
    if cfg.gap_minutes == 0 {
        return Err(rejected(
            "`gap_minutes: 0` would end an episode between every pair of steps",
        ));
    }
    if cfg.max_block_chars == 0 {
        return Err(rejected("`max_block_chars: 0` would seal empty blocks"));
    }
    if cfg.map_max_tokens <= 0 || cfg.reduce_max_tokens <= 0 {
        return Err(rejected(
            "`map_max_tokens` and `reduce_max_tokens` must be positive",
        ));
    }
    // V2: "every block carries refs into the raw beneath it". `max_evidence_refs: 0` is a config
    // that makes every block carry none — an index that indexes nothing — and it must fail at
    // boot rather than quietly produce blocks that break the property.
    if cfg.max_evidence_refs == 0 {
        return Err(rejected(
            "`max_evidence_refs: 0` would seal blocks that name no raw evidence at all; a tier is \
             an INDEX over the raw beneath it (§3)",
        ));
    }
    if cfg.max_notable_refs == 0 {
        return Err(rejected(
            "`max_notable_refs: 0` would seal every block as notable to everyone (P1-D13), which \
             is the empty set's meaning and not a bound anyone chose",
        ));
    }
    // P4-D11's lag is a FLOOR: zero would let a pass seal the steps the verbatim tail is still
    // showing, so a sealed tier and the tail would describe the same steps.
    if cfg.seal_lag_steps == 0 {
        return Err(rejected(
            "`seal_lag_steps: 0` would seal into the verbatim tail; a sealed tier and the tail \
             must never describe the same steps (P4-D11)",
        ));
    }
    Ok(())
}

/// The tier shape this row builds, derived from the validated config.
pub fn tier_cfg(cfg: &SummarizerConfig) -> TierCfg {
    TierCfg {
        fanout: cfg.fanout,
        max_tier: cfg.max_tier,
        lag: cfg.seal_lag_steps,
        max_window_steps: cfg.max_window_steps,
    }
}

/// The episode cut this row uses, derived from the validated config.
pub fn window_cfg(cfg: &SummarizerConfig) -> WindowCfg {
    WindowCfg {
        gap: Duration::minutes(cfg.gap_minutes as i64),
        max_steps: cfg.max_window_steps,
        min_steps: cfg.min_window_steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SummarizerConfig {
        SummarizerConfig {
            prompt_ver: crate::prompts::R4_1.to_string(),
            gap_minutes: 45,
            max_window_steps: 10,
            min_window_steps: 2,
            fanout: 10,
            max_tier: 3,
            seal_lag_steps: 20,
            max_calls_per_pass: 8,
            max_notable_refs: 12,
            max_evidence_refs: 24,
            max_block_chars: 1200,
            map_max_tokens: 1024,
            reduce_max_tokens: 1536,
        }
    }

    #[test]
    fn the_shipped_row_validates() {
        validate(&cfg()).expect("the bundle row is valid");
    }

    /// A stamp that names a prompt the binary does not have is a lie, and it fails at BOOT rather
    /// than at the first seal.
    #[test]
    fn validate_refuses_a_prompt_ver_the_binary_does_not_have() {
        let mut c = cfg();
        c.prompt_ver = "r9.9".into();
        let err = validate(&c).expect_err("an unknown prompt version is refused");
        assert!(
            err.to_string().contains("r9.9"),
            "the refusal must name it: {err}"
        );
        c.prompt_ver = String::new();
        assert!(validate(&c).is_err(), "and an empty one too");
    }

    #[test]
    fn validate_refuses_a_fanout_below_two() {
        let mut c = cfg();
        c.fanout = 1;
        let err = validate(&c).expect_err("fanout 1 is refused");
        assert!(err.to_string().contains("fanout"), "{err}");
        c.fanout = 0;
        assert!(validate(&c).is_err());
    }

    /// The three fields that used to be accepted at any value, including the values that break
    /// the properties the row exists to hold.
    #[test]
    fn validate_refuses_the_degenerate_bounds() {
        for (name, mutate) in [
            (
                "max_evidence_refs",
                (|c: &mut SummarizerConfig| c.max_evidence_refs = 0) as fn(&mut SummarizerConfig),
            ),
            ("max_notable_refs", |c| c.max_notable_refs = 0),
            ("seal_lag_steps", |c| c.seal_lag_steps = 0),
        ] {
            let mut c = cfg();
            mutate(&mut c);
            let err = validate(&c)
                .expect_err(&format!("`{name}: 0` must fail the boot"))
                .to_string();
            assert!(err.contains(name), "the refusal must name the field: {err}");
        }
    }

    #[test]
    fn the_derived_shapes_carry_the_row_values() {
        let c = cfg();
        assert_eq!(tier_cfg(&c).fanout, 10);
        assert_eq!(tier_cfg(&c).lag, 20);
        assert_eq!(window_cfg(&c).gap, Duration::minutes(45));
        assert_eq!(window_cfg(&c).max_steps, 10);
    }
}
