//! Invariant (§0.2): the judge's standing instruction is VERSIONED. `judge_prompt_ver` names an
//! entry here, `resolve::validate` refuses a boot whose config names one that is absent, and
//! editing the text without adding a version is therefore a change the catalog cannot express —
//! the same rule `rollups-summarizer`'s prompt catalog holds.

/// The catalog: `(version, system prompt)`.
pub const PROMPTS: &[(&str, &str)] = &[(RECON_1, RECON_1_SYSTEM)];

/// The version shipped in `bough-base`.
pub const RECON_1: &str = "recon-1";

/// `recon-1`. A pass may only ever SURFACE a disagreement as a proposal; §8 makes the
/// accept/reject surface Phase 5's, so the prompt never asks for a resolution.
pub const RECON_1_SYSTEM: &str = "\
You are checking two pieces of recorded evidence for a factual contradiction. \
Answer with a single line starting with the word CONTRADICTION followed by one sentence naming \
what disagrees, or the single word CLEAR if they are merely different, complementary, or about \
different things. Do not resolve the disagreement and do not speculate beyond what is written.";

/// The system prompt at `ver`, or `None` when the catalog does not carry it.
pub fn system(ver: &str) -> Option<&'static str> {
    PROMPTS.iter().find(|(v, _)| *v == ver).map(|(_, p)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_version_resolves_and_an_unknown_one_does_not() {
        assert!(system(RECON_1).is_some());
        assert!(system("recon-0").is_none());
    }
}
