//! Invariant: a prompt version is a KEY, not a label. Every `prompt_ver` a row may stamp resolves
//! to a prompt compiled into this binary; the config validator refuses anything else, so a sealed
//! block's stamp always names text that existed.

use crate::call::Phase;

/// The prompt catalog: `(phase, version) -> system prompt`.
pub const PROMPTS: &[(Phase, &str, &str)] = &[];

/// The prompt for one `(phase, version)`, or `None`.
pub fn lookup(_phase: Phase, _ver: &str) -> Option<&'static str> {
    todo!("WP-2: prompt lookup")
}

/// Every version this binary can stamp, for the validator's error message.
pub fn versions() -> Vec<&'static str> {
    PROMPTS.iter().map(|(_, v, _)| *v).collect()
}
