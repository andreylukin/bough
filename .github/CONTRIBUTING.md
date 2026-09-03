# Contributing to bough

bough is an alternative harness **design**. The best contributions sharpen or falsify
that design.

## Before you start

- [`README.md`](../README.md) is the overview; [`go/README.md`](../go/README.md) is the
  reference for the kernel, the service keys, the loop, history, and the test layers.
- Behavior attaches as a plugin row, not as a change to the loop. If your change needs the
  kernel or the loop to know something new, open an issue first.

Bugs, typos, flaky tests, better error messages: just send the PR.

## Setup

```
cd go
go build ./cmd/bough      # the binary
go test -race ./...       # every layer that does not need a browser
```

Go 1.27 or newer. The Playwright layer (`go/tests/web`) needs Node and a Chromium; see the
Testing section of `go/README.md`.

## The bar for a pull request

1. **`go vet ./...` and `go test -race ./...` pass.** Red PRs aren't reviewed.
2. **New behavior has a test**, offline and hermetic, with a deterministic LLM (`llm-echo`
   or a JS provider). If it's only visible in the TUI, cover it in the teatest layer or in
   `internal/vtreal`, because data-only assertions have let broken rendering ship more than
   once.
3. **Surgical diffs, one logical change.** No drive-by reformatting. Split anything whose
   title needs an "and".
4. **You have verified it yourself.** See below.

## AI-generated code

This project is a coding agent. Using one to work on it is expected, and there is no
disclosure ritual.

What is not acceptable is unverified output. **You are the author of every line you open
a PR with**, however it was produced. Before you push, you must have:

- read the whole diff and be able to explain why each part is there;
- run the tests locally, rather than trusting a claim that they pass;
- confirmed the change actually does what the PR says, by exercising it, not by reading
  a summary of it.

Tells that a PR was not verified: tests that assert nothing, or that were adjusted until
they passed; invented API, config keys, or file paths; a plausible fix for a
misdiagnosed cause; comments narrating code that isn't there; a diff far larger than the
problem. These get closed rather than reviewed, because an unverified PR moves the work onto
maintainers, and there are more people generating patches than reading them.

If you're unsure whether something is right, say so in the PR. "I think this is the
cause but I couldn't reproduce it" is a useful contribution. A confident wrong
description of a change is not.

## Commits

Conventional prefixes, scoped to the plugin or package:

```
ui: fold thinking blocks in the transcript
loop: a code error no longer ends the turn
```

Rebase on `main`; don't merge it in.

## Review

[`CODEOWNERS`](CODEOWNERS) are requested automatically. Expect a first
response within a week; ping the PR if it goes quiet longer.

Reviewers: say what would make the change mergeable. "Not the right approach" without an
alternative is not a review.

## Bugs and security

Use the issue templates, and include the exact sequence that reproduces it. bough is a
TUI, and "it looked wrong" isn't actionable without the keystrokes.

Security issues do **not** go in the tracker: see [`SECURITY.md`](SECURITY.md).

## License

Contributions are licensed under Apache 2.0, as in [`LICENSE`](../LICENSE). No CLA.
