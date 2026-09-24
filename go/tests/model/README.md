# Model-based tests: adding a flow

A **flow** is one piece of bough's behaviour written down as a small
FizzBee spec and then checked against the real thing three ways:

1. **fizz** model-checks the spec: its invariants hold in every
   reachable state.
2. **Go MBT**: the fizzbee-mbt runner walks random paths through the
   spec's state graph against a real `bough serve` (internal/servetest,
   with llm-control as the model), and after every step compares the
   state the adapter reads off the server with the spec's state.
3. **Browser**: every path the generator covers is walked in Chromium
   against a real serve; at every node `readUiState(page)` must equal
   the spec's state, plus the invariants every screen owes (no sideways
   scroll, no console errors, the status visible). The transcripts those
   walks write are then replayed against the graph (**trace check**).

The worked example is the toy session lifecycle (idle → running →
done | error, unseen cleared when viewed). Copy it:

| file | what it is |
|---|---|
| [`specs/example.fizz`](specs/example.fizz) | the spec |
| [`testdata/example/`](testdata/example/) | its state graph and `paths.json`, generated |
| [`mbt/example_test.go`](mbt/example_test.go) | Go adapter, MBT test, wrong-adapter test, history projection |
| [`../web/specs/model/example.spec.ts`](../web/specs/model/example.spec.ts) | browser flow: `init`, actions, `readUiState` |
| [`mbt/harness_test.go`](mbt/harness_test.go), [`../web/helpers/model.ts`](../web/helpers/model.ts), [`../web/helpers/control.ts`](../web/helpers/control.ts) | shared; a flow does not edit them |

What the tools write and how the graph is encoded is in
[`GRAPH.md`](GRAPH.md).

## The recipe

`<flow>` is the flow's name: lowercase letters, digits and `_` only
(it is a file name, a Go identifier fragment and a test title). A flow
adds exactly these files and edits nothing shared:

```
go/tests/model/specs/<flow>.fizz
go/tests/model/testdata/<flow>/nodes_000000_of_000000.pb           generated
go/tests/model/testdata/<flow>/adjacency_lists_000000_of_000000.pb generated
go/tests/model/testdata/<flow>/paths.json                          generated
go/tests/model/mbt/<flow>_test.go
go/tests/web/specs/model/<flow>.spec.ts
```

Every command below runs from the repo root unless it says `cd`.
Set these once per shell (the first call downloads the pinned tools):

```sh
export FIZZ_BIN="$(scripts/fizz.sh path fizz)"
export FIZZBEE_MBT_SERVER="$(scripts/fizz.sh path mbt-server)"
export FIZZBEE_MBT_BIN="$(scripts/fizz.sh path mbt-runner)"
```

### 1. The spec: `specs/<flow>.fizz`

Start from `specs/example.fizz`. Rules, each learned the hard way:

- **All state lives in a role**, with a top-level `action Init` that
  creates it (`session = Session()`). fizzbee-mbt compares only role
  state, and its server panics on a spec whose state is only global. A
  role's fields are named `<Role>#<i>.<field>` everywhere downstream
  (`Session#0.status`), its actions `<Role>#<i>.<Action>`
  (`Session#0.Prompt`). Two sessions are two instances, `Session#0` and
  `Session#1`.
- Every action is `atomic action` with its preconditions as `require`.
- Every field must be observable twice: by the Go adapter (from the
  server's API, or tracked by the adapter when it is the client's own
  state, like `viewing`) and by `readUiState` from the DOM.
- The spec's claims are `always assertion`s. They are what fizz proves.
- Keep it small: every generated path is one browser test. Aim for
  fewer than ~30 states; the example has 9 states, 20 transitions, 12
  paths.

Check it and write the fixtures:

```sh
scripts/model-test.sh gen <flow>
```

It prints `model-test: specs/<flow>.fizz: PASSED: …` and the coverage
line (`states 9/9, transitions 20/20, paths 12`). `fizz` itself exits
0 on a failed check; the script reads the `PASSED:` line for you.

**Red first:** break one assertion on purpose (make an action set a
field the assertion forbids), rerun `gen`, and capture the
`FAILED: Model checker failed. Invariant: <name>` line and
`model-test: fizz check of specs/<flow>.fizz FAILED`. Restore the spec
and rerun `gen`.

`TestSpecFixtures` fails whenever `testdata/<flow>/` is not the graph
of the current spec, and the generator's `node --test` fails whenever
`paths.json` drifted: after any edit to the spec, rerun `gen` and
commit all four generated/edited files together.

### 2. Go MBT: `mbt/<flow>_test.go`

Copy `mbt/example_test.go` and rename `example` → `<flow>` in every
identifier. What each part must do:

- The adapter is **both** the `fmbt.Model` and the role: `GetRoles`
  returns `{RoleName: "<Role>", Index: 0}: a`, and `GetState` returns
  the role's fields by bare name (`status`, not `Session#0.status`),
  values as the spec has them (strings, bools, ints). An adapter whose
  `GetRoles` is empty is never checked at all: every run passes.
- `Init` makes a fresh starting state **in the same serve** (the serve
  is started once in `new<Flow>Adapter`): new session(s), reset the
  adapter's own fields, `a.gate.reset()`.
- Each action starts with `if !a.gate.pass(<the spec's require>) {
  return nil }`, reading the require off the adapter's view. The runner
  picks actions at random, disabled ones included, and stops validating
  a walk at its first disabled action; the gate turns that action and
  everything after it into no-ops, so walks spend no real turns on
  steps nobody checks.
- An action that starts a model turn queues it with llm-control first
  (`control.Queue(..., control.Turn{Mode: "block"})`), sends the input,
  waits `control.WaitTaken`, then waits for the row to say `running`. A
  turn ends with `control.Release` (success) or `control.ReleaseWith(...,
  control.Turn{Mode: "error", ...})` (failure), then waits for the row.
  Turn names must be unique across walks (one queue per serve).
- Every wait returns an error instead of calling `t.Fatal`: inside an
  action the runner reports the failing step.
- `Cleanup` releases a turn the walk left held.
- `<flow>Options()`: `max-seq-runs` high enough that the wrong-adapter
  test below cannot miss (the example uses 100 walks of up to 6
  actions, ~10 s); **never** set `seq-seed` there: with a seed the
  runner replays exactly one walk.
- `<flow>History` projects a transcript (`[]history.Entry`) to
  `[]tracecheck.Step` using the qualified names (`Session#0.Prompt`,
  `{"Session#0.status": "running"}`), first step `Init`. Register it in
  `func init() { historyProjections["<flow>"] = <flow>History }`.
- `Test<Flow>` runs `runMBT` and then checks every transcript the walks
  wrote with `checkHistory`.
- `Test<Flow>CatchesWrongAdapter` sets one deliberate wiring bug on the
  adapter (the example releases `Fail` as a success) and requires the
  run to **fail**. Without it a green `Test<Flow>` proves nothing.

Run:

```sh
cd go && go test -count=1 -race -parallel 4 -p 4 -run 'Test<Flow>|TestSpecFixtures' ./tests/model/mbt/ -v 2>&1 | grep -E '^(---|ok|FAIL)|Error:'
```

MBT runs take a machine-wide lock (the runner can only dial the graph
server on port 50051), so several flows' tests queue behind each other
rather than collide. A failing run prints `To retry the same trace, use:
--seq-seed=<n>`; replay it with `go test ... -run Test<Flow>$ -args
--seq-seed=<n>`.

**Red first — capture both:**

1. Before `GetRoles` returns the role (or with the gate reading the
   wrong require), `Test<Flow>CatchesWrongAdapter` fails with
   `a run whose … passed; the runner is not checking state`.
2. With the adapter right, a one-line spec mutation that disagrees with
   the product (e.g. `self.unseen = False` in `Finish`) makes
   `Test<Flow>` fail with `Error: Found state mismatch for Role: …
   field: <field>, expected: …, actual: …`. Restore the spec.

If `Test<Flow>` fails **without** a mutation, the product disagrees
with the spec: that is the finding. Decide which one is wrong, fix the
product with the smallest change (test first), or the spec if the spec
was wrong, and say which in the commit.

### 3. Browser: `../web/specs/model/<flow>.spec.ts`

Copy `example.spec.ts`. `modelTests({...})` from `helpers/model.ts`
makes one test per path in `testdata/<flow>/paths.json`, starts a serve
per test (`config: CONTROL_CONFIG` for llm-control), and at every node
polls `read` until it equals the spec's role state, then checks the
global invariants. The flow supplies:

- `spec: '<flow>'`, `role: '<Role>#0'`.
- `init(page, serve)`: create what the spec's `Init` has (sessions via
  `serve.newSession()`), navigate, return a context object.
- `actions`: one per action, keyed by the bare name (`Prompt`), driven
  **through the page** the way a person would (click the row, type in
  `#composer`, press Send). An action with no UI in the current state
  may use `serve.api` the way another client would, and must say so in
  a comment. Model turns go through `helpers/control.ts` exactly as in
  Go (`queue`, `waitTaken`, `release`, `releaseWith`).
- `read` = `readUiState(c)`: every role field, read from the DOM only —
  accessible names, `aria-current`, visible classes, `location.hash`.
  Never from the API: then the test would not be about the page.
- `status(c)`: the locator that carries the state; it must be visible
  at every node.
- `sessions(c)`: the session ids whose transcripts the trace check
  replays. `cleanup(c)`: release anything held.

The page's timers run on Playwright's clock: before every read the
harness moves it 5 s, so the list poll (4 s, 12 s while a session is
open) runs without the walk waiting for it in real time.

Run:

```sh
cd go && go build -o "$TMPDIR/bough-model" ./cmd/bough
cd go/tests/web && npm ci && npx playwright install chromium   # once
BOUGH_BIN="$TMPDIR/bough-model" MODEL_TRACE_DIR="$TMPDIR/model-traces" npx playwright test specs/model/<flow>.spec.ts
```

**Red first:** break the product the spec is about (the example removed
the page's ack: `if (true || !viewingUnseen …) return;` in
`web/src/app.tsx`, then `bun run build` in `go/internal/serve/web` and
rebuild the binary) and capture the failing diff, e.g.

```
Error: step 3 (View): state
-   "unseen": false,
+   "unseen": true,
```

Then restore **both** `src/app.tsx` and `dist/app.js`
(`git checkout -- go/internal/serve/web/src/app.tsx go/internal/serve/web/dist/app.js`)
— flow branches never commit `dist/app.js`.

### 4. Trace check

After the browser run above:

```sh
cd go && MODEL_TRACE_DIR="$TMPDIR/model-traces" go test -count=1 -run '^TestHistoryTraces$' ./tests/model/mbt/ -v | grep -E '^(---|ok|FAIL)|    ---'
```

Every transcript under `$MODEL_TRACE_DIR/<flow>/` is projected with
`<flow>History` and replayed on the graph. **Red first:** copy one
transcript into another dir with the `"kind":"done"` lines removed
(`grep -v '"kind":"done"'`) and point `MODEL_TRACE_DIR` at it; capture
`history trace is not a path in the model: step N (…): action "…" is
not enabled`.

### 5. Everything

```sh
scripts/model-test.sh
```

runs the fetch script's checks, fizz on every spec, `go test
./tests/model/...` (with the tools exported), the generator's tests,
the Playwright model specs and the trace check, in that order, and
stops at the first failure. CI's `model` job runs the same script; the
`web-e2e` job also runs `specs/model/` with the rest of the browser
suite.

## What to report

For each flow: the spec's state and transition counts (the `gen` line),
the red-first output of each of the four steps above, and any product
fix with the test that was red before it.
