//! Invariant: every signal is a PURE function of step data, so the whole signal surface is
//! unit-tested without a ledger — and a signal that cannot be computed yet reports
//! [`crate::SignalState::Inactive`] rather than a zero that reads like a measurement (§16).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use bough_plugin_ledger::{AgentName, SeqRange, Step};
use tiktoken_rs::CoreBPE;

use crate::{DriftConfig, DriftFlag, Signals, Stat, ToolShare};

/// The step kind the thought-length signal reads.
pub const THOUGHT_TEXT: &str = "thought/text";
/// The step kind the tool-use signal reads.
pub const TOOL_CALL: &str = "tool/call";

/// o200k_base, the encoder §5 already measures the projection in.
fn bpe() -> &'static CoreBPE {
    static BPE: OnceLock<CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::o200k_base().expect("o200k_base is embedded in tiktoken-rs"))
}

/// Token count of `text` under o200k_base.
pub fn tokens(text: &str) -> usize {
    bpe().encode_ordinary(text).len()
}

/// Mean, variance, coefficient of variation and the two percentiles of a sample.
///
/// Population variance, not sample variance: the window IS the population being described, and a
/// Bessel correction on a window of one would report `NaN` where the honest answer is 0.
pub fn stat(samples: &[usize]) -> Stat {
    if samples.is_empty() {
        return Stat {
            n: 0,
            mean: 0.0,
            variance: 0.0,
            cv: 0.0,
            p50: 0.0,
            p95: 0.0,
        };
    }
    let n = samples.len();
    let mean = samples.iter().map(|s| *s as f64).sum::<f64>() / n as f64;
    let variance = samples
        .iter()
        .map(|s| {
            let d = *s as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / n as f64;
    // A zero mean makes the coefficient of variation undefined; 0.0 is the only value that does
    // not read as "unstable", and a window of empty thoughts is not evidence of drift.
    let cv = if mean > 0.0 {
        variance.sqrt() / mean
    } else {
        0.0
    };
    let mut sorted: Vec<usize> = samples.to_vec();
    sorted.sort_unstable();
    Stat {
        n,
        mean,
        variance,
        cv,
        p50: percentile(&sorted, 0.50),
        p95: percentile(&sorted, 0.95),
    }
}

/// Nearest-rank percentile over an already sorted sample.
fn percentile(sorted: &[usize], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (q * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1] as f64
}

/// The o200k token length of every `thought/text` step's text, in seq order.
pub fn thought_lengths(steps: &[Step]) -> Vec<usize> {
    steps
        .iter()
        .filter(|s| s.kind.as_str() == THOUGHT_TEXT)
        .map(|s| tokens(s.body.get("text").and_then(|v| v.as_str()).unwrap_or("")))
        .collect()
}

/// Tool-use distribution over `tool/call` steps, most-used first.
///
/// Ties break on the tool name, so the distribution is a FUNCTION of the window and two runs over
/// the same steps cannot render two different orders.
pub fn shares(steps: &[Step]) -> Vec<ToolShare> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for s in steps.iter().filter(|s| s.kind.as_str() == TOOL_CALL) {
        let name = s
            .body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("<unnamed>")
            .to_string();
        *counts.entry(name).or_default() += 1;
    }
    let total: usize = counts.values().sum();
    let mut out: Vec<ToolShare> = counts
        .into_iter()
        .map(|(tool, calls)| ToolShare {
            tool,
            calls,
            share: if total == 0 {
                0.0
            } else {
                calls as f64 / total as f64
            },
        })
        .collect();
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.tool.cmp(&b.tool)));
    out
}

/// Normalised Shannon entropy: 0.0 for one tool, 1.0 for uniform use.
///
/// Normalised by `ln(k)` over the tools actually seen, so the number is comparable between an
/// agent with three tools and an agent with thirty. Fewer than two tools has no spread to measure
/// and is 0.0 by definition, not by an accident of the arithmetic.
pub fn entropy(shares: &[ToolShare]) -> f64 {
    let k = shares.len();
    if k < 2 {
        return 0.0;
    }
    let h: f64 = shares
        .iter()
        .filter(|s| s.share > 0.0)
        .map(|s| -s.share * s.share.ln())
        .sum();
    (h / (k as f64).ln()).clamp(0.0, 1.0)
}

/// What the signals flag, given the thresholds.
///
/// TOO FEW SAMPLES IS EXCLUSIVE: under the floor the other two flags would be noise dressed as a
/// finding, so the only thing said is that not enough was seen (§16).
pub fn flags(signals: &Signals, cfg: &DriftConfig) -> Vec<DriftFlag> {
    if signals.samples < cfg.min_samples {
        return vec![DriftFlag::TooFewSamples];
    }
    let mut out = Vec::new();
    if signals.thought_len.n > 0 && signals.thought_len.cv > cfg.thought_len_cv_flag {
        out.push(DriftFlag::ThoughtLengthUnstable);
    }
    // An agent that called no tool has not COLLAPSED onto one; it has nothing to distribute.
    if !signals.tool_use.is_empty() && signals.tool_entropy < cfg.tool_entropy_flag {
        out.push(DriftFlag::ToolUseCollapsed);
    }
    out
}

/// Every signal for one window. PURE: the ledger read happens in the caller.
pub fn compute(agent: &AgentName, window: SeqRange, steps: &[Step], cfg: &DriftConfig) -> Signals {
    let thought_len = stat(&thought_lengths(steps));
    let tool_use = shares(steps);
    let tool_entropy = entropy(&tool_use);
    // A sample is one measured step: a thought whose length was counted, or a tool call whose
    // name entered the distribution. Steps neither signal reads are not evidence of stability.
    let samples = thought_len.n + tool_use.iter().map(|t| t.calls).sum::<usize>();
    let mut signals = Signals {
        agent: agent.clone(),
        window,
        samples,
        thought_len,
        tool_use,
        tool_entropy,
        flags: Vec::new(),
    };
    signals.flags = flags(&signals, cfg);
    signals
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, Ref, Seq, StepId, StepType, TrajId, WakeId};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn cfg() -> DriftConfig {
        DriftConfig {
            window_steps: 500,
            min_samples: 4,
            thought_len_cv_flag: 1.2,
            tool_entropy_flag: 0.35,
            max_evidence_cites: 24,
            max_state_chars: 400,
        }
    }

    fn step(seq: u64, kind: &str, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("t1"),
            seq: Seq(seq),
            at: chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant"),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(body),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::<Ref>::new()),
            ignorable: false,
        }
    }

    fn thought(seq: u64, text: &str) -> Step {
        step(
            seq,
            THOUGHT_TEXT,
            serde_json::json!({ "text": text, "step_index": 0 }),
        )
    }

    fn call(seq: u64, name: &str) -> Step {
        step(
            seq,
            TOOL_CALL,
            serde_json::json!({ "call": "c", "name": name, "args": {}, "render": "inline", "step_index": 0 }),
        )
    }

    fn win() -> SeqRange {
        SeqRange {
            from: Seq(1),
            to: Seq(99),
        }
    }

    /// The textbook sample: mean 5, population variance 4, sd 2, cv 0.4.
    #[test]
    fn stat_computes_variance_and_cv() {
        let s = stat(&[2, 4, 4, 4, 5, 5, 7, 9]);
        assert_eq!(s.n, 8);
        assert!((s.mean - 5.0).abs() < 1e-9, "{s:?}");
        assert!((s.variance - 4.0).abs() < 1e-9, "{s:?}");
        assert!((s.cv - 0.4).abs() < 1e-9, "cv is sd/mean: {s:?}");
        // Nearest rank: p50 is the 4th of 8, p95 the 8th.
        assert!((s.p50 - 4.0).abs() < 1e-9, "{s:?}");
        assert!((s.p95 - 9.0).abs() < 1e-9, "{s:?}");

        // A constant sample has no spread, and an empty one asserts nothing.
        let flat = stat(&[7, 7, 7]);
        assert_eq!((flat.variance, flat.cv), (0.0, 0.0));
        assert_eq!(stat(&[]).n, 0);
        // A window of empty thoughts has mean 0, and cv stays 0 rather than becoming NaN.
        assert!(stat(&[0, 0]).cv.is_finite());
    }

    #[test]
    fn entropy_is_zero_for_one_tool_and_one_for_uniform_use() {
        let one = shares(&[call(1, "bash"), call(2, "bash")]);
        assert_eq!(one.len(), 1);
        assert_eq!(entropy(&one), 0.0);

        let uniform = shares(&[call(1, "bash"), call(2, "read")]);
        assert!((entropy(&uniform) - 1.0).abs() < 1e-9, "{uniform:?}");

        let four_uniform = shares(&[call(1, "a"), call(2, "b"), call(3, "c"), call(4, "d")]);
        assert!((entropy(&four_uniform) - 1.0).abs() < 1e-9);

        // Skewed use lands strictly between the two ends.
        let skewed = shares(&[
            call(1, "bash"),
            call(2, "bash"),
            call(3, "bash"),
            call(4, "read"),
        ]);
        let h = entropy(&skewed);
        assert!(
            h > 0.0 && h < 1.0,
            "a skewed distribution is neither end: {h}"
        );

        assert_eq!(entropy(&[]), 0.0);
    }

    #[test]
    fn flags_need_min_samples() {
        // ONE window, unstable on both real signals: two tiny thoughts and one enormous one
        // (cv > 1.2), and every call to the same tool (entropy 0.0).
        let drifting = vec![
            thought(1, "x"),
            thought(2, "y"),
            thought(3, &"word ".repeat(400)),
            call(4, "bash"),
            call(5, "bash"),
        ];

        // Under the floor, the ONLY thing said is that too few samples were seen — the two real
        // flags are suppressed rather than reported over a window too thin to support them.
        let mut thin_cfg = cfg();
        thin_cfg.min_samples = 6;
        let thin = compute(&AgentName::new("a"), win(), &drifting, &thin_cfg);
        assert_eq!(thin.samples, 5);
        assert_eq!(thin.flags, vec![DriftFlag::TooFewSamples]);

        // The same window, one sample above the floor: both real flags appear.
        let enough = compute(&AgentName::new("a"), win(), &drifting, &cfg());
        assert_eq!(enough.samples, 5);
        assert!(!enough.flags.contains(&DriftFlag::TooFewSamples));
        assert!(
            enough.flags.contains(&DriftFlag::ThoughtLengthUnstable),
            "cv {} must exceed the threshold: {enough:?}",
            enough.thought_len.cv
        );
        assert!(
            enough.flags.contains(&DriftFlag::ToolUseCollapsed),
            "{enough:?}"
        );

        // A steady agent above the floor is flagged for nothing at all.
        let steady = compute(
            &AgentName::new("a"),
            win(),
            &[
                thought(1, "one two three four"),
                thought(2, "one two three five"),
                call(3, "bash"),
                call(4, "read"),
            ],
            &cfg(),
        );
        assert_eq!(steady.flags, Vec::<DriftFlag>::new(), "{steady:?}");
    }

    #[test]
    fn thought_length_variance_is_computed_from_thought_text_steps() {
        let steps = vec![
            thought(1, "short"),
            call(2, "bash"),
            thought(
                3,
                "a rather longer thought than the first one, with more words in it",
            ),
            step(
                4,
                "wake/start",
                serde_json::json!({ "urgency": "immediate" }),
            ),
        ];
        let lens = thought_lengths(&steps);
        assert_eq!(
            lens.len(),
            2,
            "only `thought/text` steps are sampled: {lens:?}"
        );
        assert_eq!(lens[0], tokens("short"));
        assert!(lens[1] > lens[0]);

        let s = stat(&lens);
        assert_eq!(s.n, 2);
        assert!(s.variance > 0.0, "two different lengths have spread: {s:?}");

        // And it is that measurement the assembled signals carry.
        let signals = compute(&AgentName::new("a"), win(), &steps, &cfg());
        assert_eq!(signals.thought_len, s);
    }

    #[test]
    fn tool_use_distribution_is_computed_from_tool_call_steps() {
        let steps = vec![
            call(1, "bash"),
            thought(2, "not a call"),
            call(3, "read"),
            call(4, "bash"),
            call(5, "bash"),
        ];
        let sh = shares(&steps);
        assert_eq!(
            sh.len(),
            2,
            "only `tool/call` steps enter the distribution: {sh:?}"
        );
        // Most-used first.
        assert_eq!(sh[0].tool, "bash");
        assert_eq!(sh[0].calls, 3);
        assert!((sh[0].share - 0.75).abs() < 1e-9, "{sh:?}");
        assert_eq!(sh[1].tool, "read");
        assert!((sh[1].share - 0.25).abs() < 1e-9, "{sh:?}");
        assert!((sh.iter().map(|s| s.share).sum::<f64>() - 1.0).abs() < 1e-9);

        // Ties break on the name, so the order is a function of the window.
        let tied = shares(&[call(1, "zed"), call(2, "abe")]);
        assert_eq!(
            tied.iter().map(|s| s.tool.as_str()).collect::<Vec<_>>(),
            vec!["abe", "zed"]
        );

        assert!(shares(&[thought(1, "x")]).is_empty());
    }
}
