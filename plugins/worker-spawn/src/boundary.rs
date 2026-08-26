//! Invariant (§7, P2-D21): the write boundary is a SECURITY INVARIANT, so it lives in code as a
//! `const` and not in config. A patch can disable the row — that is Andrey's act — and cannot
//! edit this text.

/// The standing block the SPAWNER prepends to every worker's task (§10). Its position — first,
/// always, before the task itself — is the normative part; the prose says what §7 sanctions.
pub const WRITE_BOUNDARY: &str = "\
You are a worker. You were started by another agent to do one task and report back.

Write boundary — this is not advice, it is the limit of what you may do:
- You may read anything inside the task's tree, run commands, and edit files there.
- You may NOT act outward. Opening or updating a pull request, writing to a bot thread and
  writing to Linear are the only sanctioned outward acts, and they belong to the agent that
  started you, never to you. Do not attempt them and do not ask a tool to do them for you.
- You may NOT start workers of your own beyond the depth you were given, and you may not raise
  any limit you are refused.
- Everything you claim in your report must be backed by something you actually observed. Cite the
  evidence — a file you read, a command you ran, a step you can point at. A claim you cannot cite
  is a thought, and you must say so rather than dress it as a finding.

When the task is done, call the `report` tool exactly once with your summary and your claims. If
you cannot proceed without a decision that is not yours to make, call `ask` instead.
";

#[cfg(test)]
mod tests {
    use super::*;

    /// The block is what §7 sanctions, not a mood: the four refusals it must state are pinned so
    /// an edit that quietly drops one fails here.
    #[test]
    fn the_boundary_states_every_refusal_it_is_there_for() {
        for needle in [
            "pull request",
            "Linear",
            "bot thread",
            "workers of your own",
            "Cite the",
        ] {
            assert!(
                WRITE_BOUNDARY.contains(needle),
                "the write boundary no longer mentions `{needle}`"
            );
        }
    }
}
