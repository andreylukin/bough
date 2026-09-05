# AGENTS.md

Briefing for anyone — person or agent — making a change here. The
overview is [`README.md`](README.md); the reference is
[`go/README.md`](go/README.md); the review bar is
[`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md). This file is the
part that is easy to get wrong from reading the code alone.

## What this is

bough is a coding agent for the terminal. The model is given one tool:
it writes a JavaScript program, bough runs it in an in-process goja VM
where every `tools.*` call is a normal function, and whatever the
program prints goes back to the model. Everything above the kernel —
the LLM provider, the loop, the tools, the UI, history, memory, MCP,
hooks, skills — is a row in `bough.yml` that can be swapped, disabled,
or hot-reloaded while a session runs.

One Go module, `go/`. 35 packages, ~51k lines. No cgo: SQLite is
`modernc.org/sqlite` and the JS runtime is goja, so one static binary
cross-compiles to every target from one runner.

## Build, test, run

Everything below runs from `go/`. Go 1.27+.

```sh
go build ./cmd/bough                 # ~2s warm
go vet ./...                         # what CI gates on first
go test -race -parallel 4 ./...      # ~3 min; plugins/ui alone is most of it
go test -race ./plugins/recipes/     # one package, under a second
```

`-parallel 4`, not the default one-per-CPU: the teatest and PTY suites
are timing-sensitive and 16-way parallelism starves them. CI passes the
same flag for the same reason.

Run it without a key — the echo provider proves the loop end to end and
touches no network:

```sh
printf 'say CODE! please\n' | go run ./cmd/bough --headless --set llm.plugin=llm-echo
```

The browser layer needs Node and a Chromium and is not part of
`go test`:

```sh
cd tests/web && npm ci && npx playwright install chromium && npm test
```

Do not run `bough update`, `bough restart`, or anything under `deploy/`
while working on the tree: the first two replace the binary you are
testing, and `deploy/` provisions a real droplet.

## Layout

| Path | What lives there |
|---|---|
| `go/kernel/` | services, events, effects, the loader, row lifecycle — the only non-plugin code besides the launcher |
| `go/cmd/bough/` | the launcher: flags, config discovery, hot reload, subcommands |
| `go/plugins/` | every behavior, one directory each; `example/` is the worked plugin from [`go/docs/PLUGINS.md`](go/docs/PLUGINS.md) |
| `go/e2e/`, `go/internal/` | headless and PTY suites, shared LLM stubs, the real-terminal suite |
| `go/tests/web/` | Playwright specs against real `bough --web` processes |
| `go/docs/` | plugin authoring, the init.js API, the memory-graph design |
| `.githooks/` | the pre-commit and commit-msg checks (`./.githooks/install`) |

## Conventions this codebase follows

**Behavior attaches as a row, never as a change to the loop.** If a
change needs the kernel or the loop to know something new, that is a
design discussion before it is a diff. Read
[`go/plugins/example/example.go`](go/plugins/example/example.go) first;
it is deliberately short and shows the four things a plugin does.

**Plugins are wired only through service keys.** No plugin imports
another plugin to reach it — it `Provide`s a key or `Get`s one, and the
kernel remounts dependents when a key lands or changes. The key table
is in [`go/README.md`](go/README.md). A new plugin package also needs
its blank import in `cmd/bough/main.go`, or its `init()` never runs and
the row fails to resolve.

**Errors name the row and wrap.** `fmt.Errorf("example-wordcount:
min_length must be a positive integer, got %v", v)`. An error out of
`Apply` marks that one row `failed` and leaves the rest of the tree
running, so it must say which row and why — it is the whole message the
user gets.

**Tests are offline, hermetic, and parallel.** Every test calls
`t.Parallel()`, takes its own `t.TempDir()` HOME, and uses a
deterministic LLM (`llm-echo`, or a JS parrot provider from `init.js`).
Nothing touches `~/.bough` or the network. Rendering is asserted in the
teatest layer or `internal/vtreal`, not only in the data — data-only
assertions have let broken rendering ship more than once.

**Comments say why, not what.** The convention throughout the tree, and
in commit messages too: the interesting content is the reason a thing is
the way it is, and what broke when it was not.

**Dependencies are added deliberately.** One module, `go/go.mod`. Pure
Go only — cgo would cost the single static binary that every install
path depends on.

## Gates a change has to pass

1. `gofmt`, `go vet ./...`, and `go test -race ./...` all clean, run by
   you, before the PR. Red PRs are not reviewed.
2. New behavior has a test at the layer where the behavior is visible.
3. One logical change per diff. No drive-by reformatting.
4. Commit subject: `scope: what changed`, lowercase-ish, 72 columns,
   no trailing period — `ui: fold thinking blocks in the transcript`.

`./.githooks/install` points this checkout at
[`.githooks/`](.githooks/), which checks 1 and 4 on every commit —
gofmt on the staged blobs, `go vet`, a scan of added lines for provider
keys, and the subject shape. `git commit --no-verify` skips them.

## Traps

**Two files written in the same millisecond tie on mtime.** Anything
that orders sessions, history files, or recipes by timestamp needs a
deterministic tiebreak, or it passes locally and fails in CI on a
filesystem with a coarser clock. `history.List` breaks ties on the id
and `recipes.Replay` mirrors it; copy that, do not re-sort on time
alone.

**Golden files are generated.** `go test ./plugins/ui -run Golden
-update` regenerates them. Hand-editing one to match a broken render is
the failure mode the layer exists to catch.

**Windows is red and does not gate.** 79 tests fail there, the job runs
`continue-on-error`, and the CI log is the todo list — see the Windows
section of [`README.md`](README.md). Do not "fix" a Windows failure by
loosening an assertion that is correct on the platforms bough ships for;
most of what fails is a test that hardcoded a POSIX path.

**Embedded files are compared byte for byte.** `.gitattributes` forces
LF checkout because a CRLF rewrite changes the bytes of files bough
`//go:embed`s and asserts on. Keep new fixtures out of the binary
exception list.

**There is no isolation boundary.** Agent programs run as you, with
your full authority — including the ones the test suite writes. That is
the design, not an oversight, and it is why nothing in the suite is
allowed to reach the network or the real `~/.bough`.
