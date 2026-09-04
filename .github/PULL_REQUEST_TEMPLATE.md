<!-- Keep one logical change per PR. If the title needs an "and", split it. -->

## What and why

<!-- What changes, and what problem it solves. Link the issue: Fixes #123 -->

## How it was verified

<!--
Not "tests pass". What did you actually do?

TUI changes: drive it for real. `go test ./internal/vtreal` runs the binary on a
real PTY, and a tmux session you actually looked at counts too. Data-only
assertions have let broken rendering ship more than once.
-->

## Checklist

- [ ] `go vet ./...` and `go test -race ./...` pass in `go/`
- [ ] New behavior has a test, and it's offline and hermetic (a deterministic
      LLM: `llm-echo`, or a JS provider from `init.js`)
- [ ] The diff is surgical, with no drive-by reformatting or unrelated refactors
- [ ] I have read the whole diff and can explain every part of it, and I ran the
      gates myself rather than trusting a claim that they pass
