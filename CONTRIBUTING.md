# Contributing to bough

Thanks for wanting to work on this. bough is an alternative harness **design**, not a
race to be a better coding agent — the most useful contributions are ones that sharpen
or falsify that design.

## Before you start

- **Read [`docs/spec.md`](docs/spec.md).** It is authoritative for product behavior.
- **Read the relevant file in [`specs/`](specs/).** Each subsystem has a behavioral
  contract pinning invariants that are not rediscoverable from the code — worker
  wind-down ordering, same-millisecond message ordering, replay determinism. Changing
  `turn/`, `harness/`, or `workflow/` without reading its spec first will waste your time
  and ours.
- **Read [`AGENTS.md`](AGENTS.md)** for the layout and the local commands.
- **Open an issue before a large change.** For anything that alters a spec, adds a host
  function, or changes the shape of a turn, discuss it first. We would rather argue about
  a paragraph than about 2,000 lines.

Small fixes — a bug, a typo, a flaky test, a clearer error message — need no issue. Just
send the PR.

## Setup

```
./scripts/setup.sh     # fresh machine
make check             # cargo check --workspace
make gates             # build + test — the pre-commit gates
make lint              # rustfmt check + clippy, warnings as errors
```

Stable Rust, one cargo workspace at the root. `make help` lists every target.

## The bar for a pull request

1. **`make gates` and `make lint` pass.** CI runs both on Linux and macOS; a PR that is
   red gets no review attention until it is green.
2. **New behavior has a test.** Tests are offline and hermetic — no network, no shared
   `$HOME`, no dependence on wall-clock ordering. If your change is only observable in
   the TUI, drive it through a real PTY (`make tui-test`) rather than asserting on the
   data behind it. Data-only assertions have repeatedly let broken rendering ship.
3. **Spec changes travel with code changes.** If you change pinned behavior, update the
   spec in the same PR. A spec that disagrees with the code is worse than no spec.
4. **Surgical diffs.** Change what the issue asks for. Drive-by reformatting, renames,
   and refactors of adjacent code make review much more expensive — send them separately
   if you think they're worth doing.
5. **One logical change per PR.** Split anything that would need "and" in its title.

## Commits

Conventional-commit prefixes, scoped to the crate:

```
feat(bough-tui): fold thinking blocks in the transcript
fix(bough-core): stop the turn runner erasing a part written outside it
test(bough-server): cover SSE reconnect after a server restart
docs: ...
```

Rebase on `main` rather than merging it in. Squash noise before you push; we do not
squash-merge for you.

## Review

Maintainers are listed in [`.github/CODEOWNERS`](.github/CODEOWNERS) and are requested
automatically. Expect a first response within a week. If a PR goes quiet for longer than
that, comment on it — that is not rude, it is helpful.

Reviewers: be concrete about what would make the change mergeable. "This is not the right
approach" without an alternative is not a review.

## Reporting bugs

Use the issue templates. The single most useful thing you can include is the exact
sequence that reproduces it and what you expected instead — bough is a TUI, and "it
looked wrong" is not actionable without the keystrokes that got you there.

Security issues do **not** go in the issue tracker. See [`SECURITY.md`](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the Apache License
2.0, as in [`LICENSE`](LICENSE). There is no CLA.
