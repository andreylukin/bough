<!-- Keep one logical change per PR. If the title needs an "and", split it. -->

## What and why

<!-- What changes, and what problem it solves. Link the issue: Fixes #123 -->

## How it was verified

<!--
Not "tests pass" — what did you actually do?
TUI changes: drive a real PTY (`make tui-test`). Data-only assertions have let broken
rendering ship more than once.
-->

## Checklist

- [ ] `make gates` passes (build + test)
- [ ] `make lint` passes (rustfmt + clippy, warnings as errors)
- [ ] New behavior has a test, and it's offline and hermetic
- [ ] If pinned behavior changed, the relevant `specs/*.md` or `docs/spec.md` changed too
- [ ] The diff is surgical — no drive-by reformatting or unrelated refactors
- [ ] I have read the whole diff and can explain every part of it, and I ran the gates
      myself rather than trusting a claim that they pass
