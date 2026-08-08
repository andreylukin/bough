# Contributing to bough

bough is an alternative harness **design**. The best contributions sharpen or falsify
that design.

## Before you start

- [`docs/spec.md`](docs/spec.md) is authoritative for product behavior.
- [`specs/`](specs/) pins per-subsystem invariants you cannot rediscover from the code.
  Read the relevant one before touching `turn/`, `harness/`, or `workflow/`.
- [`AGENTS.md`](AGENTS.md) has the layout and the commands.

Open an issue first for anything that changes a spec, adds a host function, or reshapes a
turn. Bugs, typos, flaky tests, better error messages: just send the PR.

## Setup

```
./scripts/setup.sh     # fresh machine
make gates             # build + test
make lint              # rustfmt + clippy, warnings as errors
```

Stable Rust, one workspace, `make help` for the rest. CI lints on the *latest* stable and
is deliberately unpinned — if CI flags what `make lint` didn't, `rustup update`.

## The bar for a pull request

1. **`make gates` and `make lint` pass.** Red PRs aren't reviewed.
2. **New behavior has a test**, offline and hermetic. If it's only visible in the TUI,
   drive a real PTY (`make tui-test`) — data-only assertions have let broken rendering
   ship more than once.
3. **Spec changes travel with the code.** A spec that disagrees with the code is worse
   than no spec.
4. **Surgical diffs, one logical change.** No drive-by reformatting. Split anything whose
   title needs an "and".
5. **You have verified it yourself.** See below.

## AI-generated code

This project is a coding agent. Using one to work on it is expected, and there is no
disclosure ritual.

What is not acceptable is unverified output. **You are the author of every line you open
a PR with**, however it was produced. Before you push, you must have:

- read the whole diff and be able to explain why each part is there;
- run `make gates` and `make lint` locally — not trusted a claim that they pass;
- confirmed the change actually does what the PR says, by exercising it, not by reading
  a summary of it.

Tells that a PR was not verified: tests that assert nothing, or that were adjusted until
they passed; invented API, config keys, or file paths; a plausible fix for a
misdiagnosed cause; comments narrating code that isn't there; a diff far larger than the
problem. These get closed rather than reviewed — an unverified PR moves the work onto
maintainers, and there are more people generating patches than reading them.

If you're unsure whether something is right, say so in the PR. "I think this is the
cause but I couldn't reproduce it" is a useful contribution. A confident wrong
description of a change is not.

## Commits

Conventional prefixes, scoped to the crate:

```
feat(bough-tui): fold thinking blocks in the transcript
fix(bough-core): stop the turn runner erasing a part written outside it
```

Rebase on `main`; don't merge it in.

## Review

[`.github/CODEOWNERS`](.github/CODEOWNERS) are requested automatically. Expect a first
response within a week — ping the PR if it goes quiet longer.

Reviewers: say what would make the change mergeable. "Not the right approach" without an
alternative is not a review.

## Bugs and security

Use the issue templates, and include the exact sequence that reproduces it — bough is a
TUI, and "it looked wrong" isn't actionable without the keystrokes.

Security issues do **not** go in the tracker: see [`SECURITY.md`](SECURITY.md).

## License

Contributions are licensed under Apache 2.0, as in [`LICENSE`](LICENSE). No CLA.
