# UX sweeps

Agents walk bough web as people with goals, report where it gets in
their way, and the fixes land like any other change. Nothing here runs
under `go test` or CI: a sweep costs model tokens and minutes of wall
time, and its findings are judgments, not assertions. What a sweep
finds that can be asserted becomes a Playwright spec in `tests/web`.

| File | What it is |
|---|---|
| `seed.sh DIR` | Builds a fixture HOME: real serve, real sessions on `deepseek-v4.1-flash` over the repos in `repos/`, as listed in `sessions.tsv` (cents, about a minute). |
| `fixture.py age DIR` | Spreads a fixture's sessions over two weeks (seed.sh runs it). |
| `fixture.py clone SRC DEST` | Copies a fixture and rewrites its absolute paths. |
| `env.sh up NAME FIXTURE` / `down NAME` | One tester's own serve on its own copy and port, under `$UX_RUN/NAME`; `BOUGH_BIN` is the binary under test. |
| `journeys.json` | Goals to walk, with the personas and viewports for each. |
| `personas.json` | Who walks them; `base` borrows timing and voice from the user-testing-agent plugin's personas. |

## Build a fixture

```sh
go/tests/ux/seed.sh ~/tmp/bough-fx        # needs OPENROUTER_API_KEY, jq
```

The fixture's `.bough/env` holds the key, so a tester's own turns run
live on the same cheap model. Keep fixtures out of the repo.

## Walk one journey by hand

```sh
export UX_RUN=~/tmp/ux-run BOUGH_BIN=$(cd go && go build -o /tmp/bough ./cmd/bough && echo /tmp/bough)
url=$(go/tests/ux/env.sh up t1 ~/tmp/bough-fx)
agent-browser --session t1 open "$url/" && agent-browser --session t1 snapshot -i
go/tests/ux/env.sh down t1
```

Testers drive agent-browser, one `--session` each, so any number run
at once; the laptop holds about six (a serve and a headless Chromium
each). `claude --chrome` is kept for what headless cannot do (the
clipboard, notifications) and runs one at a time.
