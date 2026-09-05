# write an AGENTS.md for this project

Study this repository and write an `AGENTS.md` at its root: the briefing
you would want if you were dropped into it cold and asked to make a
change today.

Work it out from the repository itself — the build files, the test
layout, the CI config, the last fifty commits, the code — not from what
projects of this kind usually look like. If an existing `AGENTS.md`,
`CLAUDE.md`, `CONTRIBUTING.md` or `README.md` already says something,
read it first and do not contradict or restate it; link to it instead.

Cover, in whatever order suits this project:

- **What it is**, in a sentence or two: what it does and who runs it.
- **How to build, test and run it** — the exact commands, verified by
  running them. If a command fails or is slow, say so.

  Only run what is safe to run: building, testing, linting, listing.
  Never run a target that deploys, publishes, releases, migrates a
  database, or touches anything outside this repository — read what it
  does and write that down instead. bough has no permission prompt
  standing between you and the command.
- **The layout** — the directories that matter and what lives in each.
  Skip the ones nobody edits.
- **Conventions this codebase actually follows** — naming, error
  handling, how tests are written, how dependencies are added. Cite a
  file that shows each one.
- **The gates a change has to pass** before it is committed.
- **Traps** — the things that have caught people, which usually show up
  as reverts, "fix" commits, or unusual comments in the code.

Keep it short enough that someone reads all of it: a page, maybe two.
Every claim must be one you checked. Prefer "run `go test ./...` in
`go/`" over "run the tests". If something is genuinely unclear from the
repository, write that down as an open question rather than guessing.

When it is written, print the file and say what you left out and why.
