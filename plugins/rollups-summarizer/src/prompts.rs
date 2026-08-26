//! Invariant: a prompt version is a KEY, not a label. Every `prompt_ver` a row may stamp resolves
//! to a prompt compiled into this binary; the config validator refuses anything else, so a sealed
//! block's stamp always names text that existed.

use crate::call::Phase;

/// The version the bundle row pins. A new prompt gets a NEW key here; the old text stays, because
/// a block sealed under it still names it (P4-D4).
pub const R4_1: &str = "r4.1";

/// A tighter revision, shipped alongside `r4.1` and pinned by no bundle row.
///
/// It exists because P4-D4 is a claim about what happens when the prompt is BUMPED, and a claim
/// nobody can exercise is not a claim: `a_prompt_ver_bump_does_not_re_open_a_sealed_range` moves
/// a live composition from `r4.1` to this and watches the sealed ranges stay sealed.
pub const R4_2: &str = "r4.2";

/// The shape every phase asks for, spelled once so the three prompts cannot drift apart.
const FORMAT: &str = "\
Answer with a recap and nothing else. No preamble, no apology, no questions.\n\
First: one paragraph of plain prose saying what happened and what it was for.\n\
Then, optionally, one section per theme:\n\
## <theme title>\n\
<one or two sentences>\n\
Never invent step ids, seq numbers or citations: the index is built from the input, not from you.";

const MAP_R4_1: &str = "\
You are writing the durable recap of ONE episode of an agent's working trajectory — a stretch of\n\
steps with no long pause in it. Someone will read this months from now instead of the raw steps,\n\
so write what a careful colleague would want kept: what was attempted, what was learned, what was\n\
decided, and what was left unfinished. Drop the mechanics nobody will need again.\n\n";

const REDUCE_R4_1: &str = "\
You are reducing several episode recaps of one agent's trajectory into a single coarser recap.\n\
Keep the through-line: the work that continued across episodes, the decisions that stuck, the\n\
questions still open. Say less about each episode than its own recap does; a coarse block earns\n\
its place by being shorter, not by being a list.\n\n";

const DIGEST_R4_1: &str = "\
You are rebuilding an agent's STANDING digest: what is true of this agent's work right now, not a\n\
history of it. Write what still holds — the commitments, the working assumptions, the shape of\n\
what it is doing — and leave out anything that has already been superseded.\n\n";

const MAP_R4_2: &str = "\
You are writing the durable recap of ONE episode of an agent's working trajectory — a stretch of\n\
steps with no long pause in it. Write only what a careful colleague would still want in a year:\n\
what was attempted, what was learned, what was decided, what is unfinished. Prefer one specific\n\
sentence to three general ones, and say nothing about the mechanics of the tools.\n\n";

const REDUCE_R4_2: &str = "\
You are reducing several episode recaps of one agent's trajectory into a single coarser recap.\n\
Keep only the through-line: work that continued across episodes, decisions that stuck, questions\n\
still open. A coarse block earns its place by being shorter than its children, never by listing\n\
them.\n\n";

const DIGEST_R4_2: &str = "\
You are rebuilding an agent's STANDING digest: what is true of this agent's work right now, not a\n\
history of it. Write only what still holds — commitments, working assumptions, the shape of what\n\
it is doing — and drop anything already superseded. If something is uncertain, say it is.\n\n";

/// The prompt catalog: `(phase, version) -> system prompt`.
pub const PROMPTS: &[(Phase, &str, &str)] = &[
    (Phase::Map, R4_1, MAP_R4_1),
    (Phase::Reduce, R4_1, REDUCE_R4_1),
    (Phase::Digest, R4_1, DIGEST_R4_1),
    (Phase::Map, R4_2, MAP_R4_2),
    (Phase::Reduce, R4_2, REDUCE_R4_2),
    (Phase::Digest, R4_2, DIGEST_R4_2),
];

/// The prompt for one `(phase, version)`, or `None`.
pub fn lookup(phase: Phase, ver: &str) -> Option<&'static str> {
    PROMPTS
        .iter()
        .find(|(p, v, _)| *p == phase && *v == ver)
        .map(|(_, _, text)| *text)
}

/// The full system text for a call: the phase's prompt plus the shared format contract.
pub fn system(phase: Phase, ver: &str) -> Option<String> {
    lookup(phase, ver).map(|head| format!("{head}{FORMAT}"))
}

/// Every version this binary can stamp, for the validator's error message.
pub fn versions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = PROMPTS.iter().map(|(_, v, _)| *v).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether every phase has a prompt at `ver`. A version that covers only the map half would seal
/// tier 1 and then fail mid-pass, which is a boot-time refusal, not a runtime surprise.
pub fn covers_every_phase(ver: &str) -> bool {
    [Phase::Map, Phase::Reduce, Phase::Digest]
        .into_iter()
        .all(|p| lookup(p, ver).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_version_covers_every_phase() {
        for v in versions() {
            assert!(covers_every_phase(v), "`{v}` does not cover every phase");
        }
    }

    #[test]
    fn an_unknown_version_resolves_to_nothing() {
        assert!(lookup(Phase::Map, "nope").is_none());
        assert!(system(Phase::Map, R4_1).is_some());
    }
}
