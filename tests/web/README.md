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

Every test is isolated: its own bough process, temp `HOME`, temp cwd,
its own copy of `bough.yml`, and a fresh free port. `llm-echo` (or a JS
provider from a test-written `init.js`) is forced via `--set`, so no
test ever calls a real API.

The Go binary is built ONCE per suite run by `helpers/global-setup.ts`
(`go build -o <repo>/bough ./cmd/bough`). To skip the build (e.g. CI
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
- `helpers/term.ts` — screen reading goes through the sip client's
  `window.sipTerm.term.buffer.active` (xterm.js-compatible buffer API;
  `getLine(i).translateToString(true)` per row). The renderer draws to
  canvas/WebGL so there are no DOM rows — the buffer is the only
  truthful screen text. `typeInTerm` clicks `#terminal` and sends real
  key events through `page.keyboard`.

## Updating

- New spec: add a file under `specs/`; use the `launchBough` fixture
  and never share a process between tests.
- sip client internals (`window.sipTerm`) come from
  `github.com/Gaurav-Gosain/sip` `static/terminal.js`; if a sip upgrade
  breaks `boot()`/`termText()`, re-check that file's exported globals.
- Keymap note: the shipped default for the history inspector is
  ctrl+o, not ctrl+h (see `plugins/ui/theme.go`).
