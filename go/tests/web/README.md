# bough web e2e tests (Playwright)

End-to-end tests for `bough --web` — the sip-served browser terminal —
driven with Playwright against a real bough process per test.

## Run

```sh
cd tests/web
npm install
npx playwright install chromium   # once
npm test                          # all specs, fully parallel
```

Every test is isolated: either its own bough process (temp `HOME`, temp
cwd, its own copy of `bough.yml`, a fresh free port), or its worker's
shared `bough serve` and only the sessions it created there. `llm-echo`
(or `llm-control`, or a JS provider from a test-written `init.js`) is
forced, so no test ever calls a real API.

Workers default to one per CPU locally and two on CI. The longest model
walks are projects of their own in `playwright.config.ts`, queued
longest first, so the run does not end on one heavy file with the other
workers idle; everything else is the `rest` project. Measure with
`npx playwright test --workers=8`.

The Go binary comes from `helpers/global-setup.ts`, which runs
`go run ./internal/testbin/boughbin`: the same cached build the Go
suites use, keyed by a hash of its sources under `$TMPDIR/bough-testbin`,
so it is linked only when the sources changed. To skip it (e.g. CI
built it already), point `BOUGH_BIN` at a prebuilt binary:

```sh
BOUGH_BIN=/path/to/bough npm test
```

## Useful invocations

```sh
npx playwright test specs/basic.spec.ts        # one file
npx playwright test -g "deny hook"             # by title
npx playwright test --headed                   # watch it
npx playwright test --shard=1/4                # CI sharding
npx playwright show-report                     # after a failure
```

Retries are 1 in CI (`CI=1`), with `trace: on-first-retry`; open a
failed run's trace with `npx playwright show-trace <trace.zip>`.

## How the helpers work

- `helpers/bough.ts` — `launch(opts)` spawns the binary with
  `--web 127.0.0.1:<freeport>`, seeds files into the temp HOME/cwd
  (`home:`/`cwd:` maps: hooks, skills, `init.js`, `AGENTS.md`, ...),
  waits for `/health` 200, and returns `{url, home, cwd, proc, kill,
  cli}`. `cli(["log"])` runs bough subcommands against the same HOME.
- `helpers/fixtures.ts` — the `launchBough` fixture kills every
  spawned process at test end and attaches the full captured
  stdout+stderr to the report when the test failed.
- `helpers/serve.ts` — a real `bough serve` on an isolated HOME. `serve`
  is one process per test (`test.use({ serveOpts: { home, config } })`
  seeds files and `~/.bough/bough.yml`, llm-echo by default);
  `sharedServe` is one per worker (`workerServeOpts`), and a test using
  it touches only the sessions it made with `newSession()`. Both sign
  the page in before the first navigation, give `api` (bearer + Origin
  preset), and attach the server log when a test fails.
  `specs/serve-fixture.spec.ts` shows both.
- `helpers/term.ts` — screen reading goes through the sip client's
  `window.sipTerm.term.buffer.active` (xterm.js-compatible buffer API;
  `getLine(i).translateToString(true)` per row). The renderer draws to
  canvas/WebGL so there are no DOM rows — the buffer is the only
  truthful screen text. `typeInTerm` clicks `#terminal` and sends real
  key events through `page.keyboard`.

## Updating

- New spec against the control room: use `sharedServe` from
  `helpers/serve.ts` and work only in sessions the test made (or mock
  the reads, as `background-agents.spec.ts` does). Take a `serve` of
  the test's own only when the test seeds `HOME` or `bough.yml`
  (`project.spec.ts`, `me.spec.ts`, `welcome.spec.ts`,
  `engine-calls.spec.ts`, most model walks) or needs an empty server.
- New spec against `bough --web`: use `launchBough`, one process per
  test. A `--web` process is one loop that every page attached to it
  shares (`multi.spec.ts`), so two tests on one would see each other's
  turns.
- No fixed sleeps: wait on the thing (`waitForTermText`, `inputUntil`,
  `expect.poll`, a response). A model walk's reads poll every
  25–250 ms (`POLL_INTERVALS` in `helpers/model.ts`).
- sip client internals (`window.sipTerm`) come from
  `github.com/Gaurav-Gosain/sip` `static/terminal.js`; if a sip upgrade
  breaks `boot()`/`termText()`, re-check that file's exported globals.
- Keymap note: the shipped default for the history inspector is
  ctrl+o, not ctrl+h (see `plugins/ui/theme.go`).
