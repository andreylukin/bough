//! How much text one tool result may put into the model's context.
//!
//! The caps this replaces were absolute: a 2 MiB view limit and a 10,000-char
//! inline extract, the same numbers whatever model was reading them. 2 MiB is
//! roughly 500k tokens — four times the whole window of a 131k model — so the
//! "limit" permitted a single `view()` to overflow the context several times
//! over. That is not a hypothetical: two subagents on `cerebras:gemma-4-31b`
//! died at 154,126 and 247,377 tokens against a 131,072 limit, having done
//! nothing more exotic than read the repo.
//!
//! A budget is only meaningful as a FRACTION OF THE WINDOW. The shares below
//! are per result, so a round making several calls still has to fit, but a
//! single call can no longer be the whole overflow by itself.
//!
//! Both shares are ceilings, never floors: a model with a huge window keeps
//! the old constants rather than being handed a bigger allowance than the
//! feature was ever tested with.

/// Chars per token, roughly, for the source and command output that actually
/// flows through here. Deliberately the standard coarse estimate — a budget
/// that needed a tokenizer to be correct would be a budget that drifts per
/// provider.
const CHARS_PER_TOKEN: usize = 4;

/// A `view()` is deliberate: the model named a file and wants to edit it, and
/// `patch()` cannot anchor to a file that was never viewed whole. It gets the
/// larger share.
const VIEW_SHARE: f64 = 0.10;

/// Command output is usually incidental — a build log, a test run — and the
/// whole of it is on disk either way, so the inline extract only has to let
/// the model recognize what it is looking at.
const SHELL_SHARE: f64 = 0.01;

/// The per-result ceilings for one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultBudget {
    /// Largest file `view()` will render.
    pub view_bytes: u64,
    /// Output above which a command spills to a file instead of inlining.
    pub spill_over_chars: usize,
    /// Verbatim head kept inline when output spills.
    pub spill_head_chars: usize,
    /// Verbatim tail kept inline when output spills.
    pub spill_tail_chars: usize,
}

/// The absolute ceilings, used when the model's window is unknown.
pub const UNBOUNDED: ResultBudget = ResultBudget {
    view_bytes: super::files::MAX_VIEW_BYTES,
    spill_over_chars: super::spill::SPILL_OVER_CHARS,
    spill_head_chars: super::spill::SPILL_HEAD_CHARS,
    spill_tail_chars: super::spill::SPILL_TAIL_CHARS,
};

/// The budget for a model with this context window, in tokens.
///
/// `None` — an unpriced or unknown model — keeps the absolute ceilings. A
/// guess at the window would be worse than not guessing: too low silently
/// starves a model that was fine, and the ceilings are what every model used
/// before this existed.
pub fn budget_for(window_tokens: Option<i64>) -> ResultBudget {
    let Some(window) = window_tokens.filter(|w| *w > 0) else {
        return UNBOUNDED;
    };
    let chars = (window as usize).saturating_mul(CHARS_PER_TOKEN);
    let share = |s: f64| (chars as f64 * s) as usize;

    let extract = share(SHELL_SHARE).min(UNBOUNDED.spill_head_chars + UNBOUNDED.spill_tail_chars);
    ResultBudget {
        view_bytes: (share(VIEW_SHARE) as u64).min(UNBOUNDED.view_bytes),
        // The 2:1 the absolute constants use — spilling is worth doing only
        // when there is meaningfully more than the extract would have kept.
        spill_over_chars: extract.saturating_mul(2),
        spill_head_chars: extract / 2,
        spill_tail_chars: extract / 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_window_keeps_the_absolute_ceilings() {
        assert_eq!(budget_for(None), UNBOUNDED);
        // A nonsense window is an unknown one, not a zero-byte budget.
        assert_eq!(budget_for(Some(0)), UNBOUNDED);
        assert_eq!(budget_for(Some(-1)), UNBOUNDED);
    }

    #[test]
    fn a_small_window_gets_a_view_limit_it_can_actually_hold() {
        // The case that motivated this: gemma-4-31b, 131,072 tokens. The old
        // 2 MiB limit was ~500k tokens — the model could be told "yes" to a
        // read four times larger than everything it can hold at once.
        let b = budget_for(Some(131_072));
        assert!(b.view_bytes < UNBOUNDED.view_bytes);
        let view_tokens = b.view_bytes as usize / CHARS_PER_TOKEN;
        assert!(
            view_tokens < 131_072 / 4,
            "one view must not be a quarter of the window: {view_tokens}"
        );
    }

    #[test]
    fn a_huge_window_is_capped_at_the_ceilings_not_handed_more() {
        let b = budget_for(Some(2_000_000));
        // The shell extract saturates: a bigger window does not mean a model
        // wants more of a build log pasted into it.
        assert_eq!(b.spill_head_chars, UNBOUNDED.spill_head_chars);
        assert_eq!(b.spill_tail_chars, UNBOUNDED.spill_tail_chars);
        assert_eq!(b.spill_over_chars, UNBOUNDED.spill_over_chars);
        // The view share still governs — 2 MiB is a backstop for a window so
        // large the fraction would exceed it, not the operative rule.
        assert!(b.view_bytes <= UNBOUNDED.view_bytes);
        assert!(b.view_bytes > budget_for(Some(131_072)).view_bytes);
    }

    #[test]
    fn the_budget_tightens_monotonically_as_the_window_shrinks() {
        let big = budget_for(Some(400_000));
        let small = budget_for(Some(64_000));
        assert!(small.view_bytes < big.view_bytes);
        assert!(small.spill_over_chars < big.spill_over_chars);
        // Head and tail stay equal — the extract is a preview from both ends.
        assert_eq!(small.spill_head_chars, small.spill_tail_chars);
    }
}
