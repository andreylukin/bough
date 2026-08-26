//! Invariant: the token estimate is o200k_base (§5) and the headroom factor is applied to the
//! configured budget, never to the measured text. The encoder is built once, in a `OnceLock`.

/// Token count of `text` under o200k_base.
pub fn count(text: &str) -> usize {
    todo!("WP-4: tokens::count")
}

/// `floor(budget_tokens * headroom)` — §5's headroom factor (config, default 0.6; P1-D20).
pub fn effective_budget(budget_tokens: usize, headroom: f32) -> usize {
    todo!("WP-4: tokens::effective_budget")
}
