---
description: Read AI-vs-human authorship for this repository with Git AI — who wrote a line, what prompt produced it, and how much of a commit or range the agent authored. Use when asked who or what wrote some code, whether a line is AI-written, what prompt produced a change, or for AI-authorship stats on a commit, branch or file.
---

# Git AI authorship

`git ai` records which lines an agent wrote and which the human wrote, and keeps the
prompt behind each one. This skill is about READING that record. The recording is done
by the `git-ai` hook in this same plugin — you never call `git ai checkpoint` yourself,
and doing so by hand would attribute the wrong lines to the wrong author.

## Before anything else

```bash
command -v git-ai || echo "git-ai is not installed"
```

If it is missing, say so and stop — every command below is unavailable, and there is no
authorship record to reason about. Git AI installs from <https://usegitai.com>.

Attribution only exists for work done while the hook was switched on and inside a git
repository. A file with no record is not evidence that a human wrote it; it is evidence
that nothing was tracked. Say which of the two you are looking at — `[no-data]` in the
output means exactly that.

## Who wrote this line

```bash
git ai blame <file>            # git blame, plus the agent or human per line
git ai diff HEAD               # one commit, each +/- line marked 🤖tool or 👤user
git ai diff main..feature      # or a range
git ai diff HEAD --json        # the same, as data
```

`git ai blame` takes git blame's options. Reach for `diff` when the question is about a
change, and `blame` when it is about the file as it stands.

## What prompt produced it

`git ai diff --json` carries a prompt id per line. Resolve it:

```bash
git ai show-prompt <prompt_id>            # most recent occurrence
git ai show-prompt <prompt_id> --commit <rev>
```

It answers with the agent, the model, the human author, the messages, and how many of the
agent's lines survived (`accepted_lines` against `overriden_lines`) — which is the honest
measure of whether that prompt's output was kept.

## How much of this is AI

```bash
git ai stats                   # HEAD
git ai stats <start>..<end>    # a range, same semantics as git log
git ai stats --json
git ai stats --ignore "*.lock" "**/dist/**"
git ai status                  # the uncommitted working changes
git ai usage --repo <name>     # the all-time local odometer
```

Exclude lockfiles and generated output before quoting a percentage; a regenerated
`Cargo.lock` will otherwise dominate the number and mean nothing.

## Reporting it

Quote the numbers with their denominator and their exclusions — "412 of 1,207 changed
lines across `main..HEAD`, lockfiles excluded" says something; "the AI wrote 34%" does
not. Acceptance rate is about the lines that SURVIVED review, so it is a claim about
usefulness rather than volume, and worth naming separately when you have it.
