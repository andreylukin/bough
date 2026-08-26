//! Invariant: the token estimate is o200k_base (§5) and the headroom factor is applied to the
//! configured budget, never to the measured text. The encoder is built once, in a `OnceLock`.

use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

static ENCODER: OnceLock<CoreBPE> = OnceLock::new();

fn encoder() -> &'static CoreBPE {
    ENCODER.get_or_init(|| {
        tiktoken_rs::o200k_base().expect("o200k_base is embedded in tiktoken-rs and always loads")
    })
}

/// Token count of `text` under o200k_base.
pub fn count(text: &str) -> usize {
    // `encode_ordinary`: projection text is data, never a control sequence, so a literal
    // "<|endoftext|>" in a step body must count as text and never as a special token.
    encoder().encode_ordinary(text).len()
}

/// `floor(budget_tokens * headroom)` — §5's headroom factor (config, default 0.6; P1-D20).
pub fn effective_budget(budget_tokens: usize, headroom: f32) -> usize {
    // NaN and any non-positive factor floor to zero: over-budget, never a wrapped-around budget.
    if !headroom.is_finite() || headroom <= 0.0 {
        return 0;
    }
    let scaled = (budget_tokens as f64) * (headroom as f64);
    scaled.floor() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_is_o200k() {
        let text = "the projection is a pure function of the ledger";
        let o200k = tiktoken_rs::o200k_base().unwrap();
        assert_eq!(count(text), o200k.encode_ordinary(text).len());

        // And it is that encoder specifically, not just "some tokenizer": o200k and cl100k
        // disagree on this string, so a silent swap would show up here.
        let cl100k = tiktoken_rs::cl100k_base().unwrap();
        let discriminating = [
            "\u{4f60}\u{597d}\u{4e16}\u{754c}",
            "                    ",
            "let projection = assemble(&request).await?;",
            "\u{1f40d}\u{1f980}",
        ]
        .iter()
        .find(|s| o200k.encode_ordinary(s).len() != cl100k.encode_ordinary(s).len())
        .copied()
        .expect("some fixture must separate the two encoders");
        assert_eq!(
            count(discriminating),
            o200k.encode_ordinary(discriminating).len()
        );
        assert_ne!(
            count(discriminating),
            cl100k.encode_ordinary(discriminating).len(),
            "count() must be o200k specifically, not just some tokenizer"
        );

        assert_eq!(count(""), 0);
    }

    #[test]
    fn headroom_factor_is_applied() {
        // §5's factor: Claude-family tokens run ~1.5-1.7x o200k on code, so the budget shrinks,
        // never the measured text.
        assert_eq!(effective_budget(160_000, 0.6), 96_000);
        assert_eq!(effective_budget(100, 1.0), 100);
        assert!(effective_budget(100, 0.6) < 100);
    }

    #[test]
    fn effective_budget_floors() {
        assert_eq!(effective_budget(7, 0.5), 3);
        assert_eq!(effective_budget(1, 0.6), 0);
        assert_eq!(effective_budget(0, 0.6), 0);
        // A non-positive factor is the assembler's `validate()` job to refuse; here it can only
        // ever floor to zero, never wrap around into a huge budget.
        assert_eq!(effective_budget(100, 0.0), 0);
        assert_eq!(effective_budget(100, -1.0), 0);
    }
}
