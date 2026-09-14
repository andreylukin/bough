# bough loop: deterministic pipelines

Status: Area A (engine) implemented, foreground phase 1. Area B is skills only.

`bough loop run pipeline.yml` walks a graph of **agent** nodes (headless bough
children) and **check** nodes (shell commands). Routing is decided by exit codes
and a strict verdict line, never by an agent. Everything the loop does is written
to a run dir.

Two areas with disjoint files:

- **Area A: the engine.** Go code, tests and this doc.
- **Area B: skills.** Markdown and shell only, with no engine change.

---

# Area A: engine (Go)

## pipeline.yml

```yaml
name: fix-until-green
goal: "Make `go test ./pkg/...` pass without changing test files."
start: coder
max_steps: 40                    # hard cap on total node visits for the run; 0 = no cap beyond max_visits

nodes:
  coder:
    type: agent
    mode: project                # local | project
    project: bough               # slug; required when mode: project
    model: llm-anthropic/claude-sonnet-5   # plugin/model -> --set llm.plugin= --set llm.model=
    session: resume              # resume (one session, re-prompted each visit) | fresh
    prompt: |
      You are the coder. Goal: {{goal}}
      Feedback from the last failing step:
      {{input}}
    next: tests                  # agent nodes route on turn end: ok -> next, errored -> fail
    fail: coder
    max_visits: 8

  tests:
    type: check
    run: go test ./pkg/... 2>&1  # sh -c in the coder's worktree (see "Checks")
    cwd: coder                   # the name of an agent node whose worktree to use, or a path; default and relative paths: the pipeline file's dir
    timeout: 10m
    pass: validator
    fail: coder                  # {{input}} for coder = last 200 lines of output
    max_visits: 8

  validator:
    type: agent
    mode: local
    model: llm-openai/gpt-6
    session: fresh
    holdout: [acceptance/*.md]   # globs, relative to the pipeline file's dir
    prompt: |
      Review the coder's diff against the acceptance criteria in {{holdout_dir}}.
      Do NOT quote the criteria. Describe failures as behaviour.
      End with exactly one line: VERDICT: PASS or VERDICT: FAIL
    verdict: true                # last non-empty line must be VERDICT: PASS|FAIL; anything else = FAIL
    pass: done
    fail: coder
    max_visits: 4

coaches:
  - name: coach
    target: coder
    model: llm-openrouter/qwen-4-coder
    every: 90s
    cooldown: 5m
    max_steers: 3
    prompt: |
      You watch a coding agent. If it is stuck, looping or off-goal, reply with ONE short
      steering sentence. Otherwise reply exactly: NONE
```

These node names are reserved as targets: `done` ends the run with status
`passed` and exit 0, and `fail` ends it with status `failed` and exit 1.

**Validation.** `pipeline.Load` rejects a pipeline in any of these cases:

- `start`, or any `next`, `pass` or `fail`, points to a node that does not exist.
- A node has no `max_visits`.
- A check node has no `pass` or `fail`.
- A node sets `verdict: true` but is not an agent.
- `holdout` files are set but some agent node without `holdout` has `mode: local` (see "Holdout").
- A coach targets something other than an agent node.

**Template variables.** Plain string replacement, not text/template:

| Variable | Value |
|---|---|
| `{{goal}}` | the pipeline's `goal` |
| `{{input}}` | the output routed into this visit: a check's output tail, or an agent's reply after the leak filter |
| `{{holdout_dir}}` | only for nodes that list `holdout` |
| `{{visit}}` | the visit number of this node |
| `{{run_dir}}` | never offered to project nodes |

**Routing.**

- Agent node, no verdict: the turn ended with `done` and not errored goes to `next`. `cancelled`, `exit` or an errored turn goes to `fail`.
- Agent node, `verdict: true`: goes to `pass` or `fail` based on the verdict line.
- Check node: exit 0 goes to `pass`; a non-zero exit or a timeout goes to `fail`.
- Visiting a node beyond `max_visits`, or exceeding `max_steps`, ends the run as `exhausted` with exit 3.
- A check that cannot start (missing `cwd`, a `cwd` agent with no session yet) records `exit -1` with the error in `output.txt` and routes to `fail`.

## Run dir: `~/.bough/loops/<run-id>/`

`run-id` is `history.NewID()`.

```
pipeline.yml          copy of the input, frozen
state.json            {id, name, status: running|passed|failed|exhausted|stopped|error,
                       node, step, visits{node:n}, sessions{node:id}, pid, started, ended,
                       reason (error only; setup failures land here too)}
runner.log            stdout+stderr of a --detach run
meta.json             supervisor meta
events.jsonl          one Event per line (below), append-only
holdout/              staged copies of holdout files (0600), never mounted anywhere
steps/<step>-<node>/
  prompt.md           exact text sent (agent)
  reply.md            final assistant reply (agent), before the leak filter
  routed.md           what was passed on as {{input}} (after the leak filter)
  output.txt          combined stdout+stderr (check)
  result.json         {node, visit, session?, exit?, verdict?, route, reason, started, ended}
coach/<n>.md          transcript tail shown to the coach, its reply, and sent|skipped + why
```

Event kinds in `events.jsonl`: `start`, `visit`, `prompt`, `reply`, `check`, `route`, `leak`, `coach`, `steer`, `end`.

## Package layout

```
go/internal/pipeline/      (named to avoid confusion with plugins/loop, the turn loop)
  pipeline.go              types, Load, Validate
  run.go                   Runner: the graph walk, run dir, routing
  agent.go                 agent node via the Sessions seam
  check.go                 check node
  coach.go                 coach goroutine
  holdout.go               staging, preflight, leak filter
  pipeline_test.go  run_test.go  coach_test.go  holdout_test.go
go/cmd/bough/loopcmd.go    `bough loop run|status|stop`
go/cmd/bough/main.go       add "loop" to commands + case
go/internal/serve/supervisor.go   CreateOptions.ID/Args/Env/Origin (below)
go/internal/serve/supervisor_test.go  test for pre-minted-id Create
go/plugins/tools/tools.go  confine `view` in project mode
go/plugins/tools/tools_test.go  (or the existing test file) view confinement test
go/e2e/loop_test.go        end-to-end with the real binary
go/docs/loops.md           this file
```

## Types and signatures

```go
package pipeline

type Pipeline struct {
	Name     string           `yaml:"name"`
	Goal     string           `yaml:"goal"`
	Start    string           `yaml:"start"`
	MaxSteps int              `yaml:"max_steps"`
	Nodes    map[string]*Node `yaml:"nodes"`
	Coaches  []Coach          `yaml:"coaches"`
	Dir      string           `yaml:"-"` // dir of the pipeline file; holdout globs resolve here
}

type Node struct {
	Name      string        `yaml:"-"`
	Type      string        `yaml:"type"`    // agent | check
	Mode      string        `yaml:"mode"`    // agent: local | project
	Project   string        `yaml:"project"`
	Model     string        `yaml:"model"`   // "plugin/model"; empty = child's bough.yml
	Session   string        `yaml:"session"` // resume | fresh (default fresh)
	Prompt    string        `yaml:"prompt"`
	Holdout   []string      `yaml:"holdout"`
	Verdict   bool          `yaml:"verdict"`
	Run       string        `yaml:"run"`     // check
	Cwd       string        `yaml:"cwd"`     // check: agent node name or path
	Timeout   time.Duration `yaml:"timeout"`
	Next      string        `yaml:"next"`
	Pass      string        `yaml:"pass"`
	Fail      string        `yaml:"fail"`
	MaxVisits int           `yaml:"max_visits"`
}

type Coach struct {
	Name      string        `yaml:"name"`
	Target    string        `yaml:"target"`
	Model     string        `yaml:"model"`
	Every     time.Duration `yaml:"every"`
	Cooldown  time.Duration `yaml:"cooldown"`
	MaxSteers int           `yaml:"max_steers"`
	Prompt    string        `yaml:"prompt"`
}

func Load(path string) (*Pipeline, error)   // parse + Validate
func (p *Pipeline) Validate() error

// Sessions is the part of *serve.Supervisor the runner uses; *serve.Supervisor satisfies it.
type Sessions interface {
	Create(opt serve.CreateOptions) (string, error)
	Send(id, text string) error
	Subscribe(id string) (<-chan serve.Event, func())
	Entries(id string) ([]history.Entry, error)
	Live(id string) bool
	PendingAsk(id string) *serve.Ask
	Kill(id string) error
}

type Options struct {
	Home     string   // explicit; tests use t.TempDir()
	Sessions Sessions
	Sets     []string // extra --set for every child (tests: llm.plugin=llm-echo)
	Out      io.Writer // human progress lines; nil = discard
	ID       string    // pre-minted run id ("" = mint one); `loop run --detach` names the run dir first
}

type Runner struct { /* p, opt, dir, state, mu */ }

func NewRunner(p *Pipeline, opt Options) (*Runner, error) // mints id, creates run dir, stages holdout, preflight
func (r *Runner) ID() string
func (r *Runner) Dir() string
func (r *Runner) Run(ctx context.Context) (State, error) // blocks; ctx cancel => status stopped, kills live node children

type State struct {
	ID       string            `json:"id"`
	Name     string            `json:"name"`
	Status   string            `json:"status"`
	Node     string            `json:"node"`
	Step     int               `json:"step"`
	Visits   map[string]int    `json:"visits"`
	Sessions map[string]string `json:"sessions"`
	Pid      int               `json:"pid"`
	Started  time.Time         `json:"started"`
	Ended    time.Time         `json:"ended,omitzero"`
}

type Event struct {
	At   time.Time      `json:"at"`
	Kind string         `json:"kind"`
	Node string         `json:"node,omitempty"`
	Step int            `json:"step,omitempty"`
	Text string         `json:"text,omitempty"`
	Data map[string]any `json:"data,omitempty"`
}

func ReadState(home, id string) (State, error)
func ListRuns(home string) ([]State, error) // newest first
func RunsDir(home string) string            // home/.bough/loops

// holdout.go
func stageHoldout(p *Pipeline, runDir string) ([]string, error)
func preflightHoldout(p *Pipeline, staged []string, home string) error
func leaks(reply string, holdout []string) (bool, string) // 8-word shingle overlap; returns the offending phrase
func verdictOf(reply string) (pass bool, ok bool)       // last non-empty line == "VERDICT: PASS"|"VERDICT: FAIL"
```

### Supervisor changes (small, `internal/serve/supervisor.go`)

```go
type CreateOptions struct {
	Cwd, Prompt, Mode, Slug string
	SpawnedBy string
	ID     string   // new: pre-minted id -> BOUGH_SESSION_ID, no dir-diff discovery
	Args   []string // new: extra argv after --headless --json (e.g. --set llm.model=x)
	Env    []string // new: extra env
	Origin string   // new: BOUGH_ORIGIN override; "" = web
}
```

- `start(ch, dir, id, extra)` takes `args []string` as well.
- When `ID` is set, `Create` skips `newSessionID` and waits for `HistDir/<ID>.jsonl`, the same way `CreateChild` does.
- Loop children get `Origin: "loop"`.
- On resume, `ensure` must reuse the recorded `Args`. The runner does not rely on that: to resume, it calls `Send`. The model is also sent once as a `/model <plugin> <model>` line immediately after create, before the prompt. That line is idempotent, so a resumed session keeps its model even if it respawned without the args.

## Execution

- **Agent visit** (`agent.go`):
  1. Get the session:
     - `session: fresh`, or the first visit: `id := history.NewID()`, then `Create({ID:id, Cwd: pipelineDir, Mode, Slug, Args: sets+model, Origin:"loop"})`, with an empty Prompt.
     - Otherwise reuse `sessions[node]`.
  2. `Subscribe(id)`, then `Send(id, prompt)`.
  3. Wait for the first `done`, `cancelled` or `exit` event. `error` events are only logged.
  4. Poll `Entries` for up to 2 s for the closing entry, then take the reply with the same logic as `serve.lastTurn`. Export it as `serve.LastTurn(entries) (reply string, errored bool)`, a thin wrapper.
  5. Any pending ask fails the visit with reason `ask`: the runner polls `PendingAsk` every 2 s and, when an ask is pending, calls `Kill`. Loop nodes are unattended.
- **Check visit** (`check.go`):
  - Runs `sh -c run` on the host with a timeout and the combined output. It does not use an orb exec.
  - `cwd: <agent>` resolves to that node's worktree: `~/.bough/orbs/<session>/<repo>` for project nodes, the pipeline dir for local nodes.
  - Env is `os.Environ()` plus `BOUGH_LOOP_RUN=<id>`.
  - Limit: checks run with the user's full privileges. They are written by the pipeline author, not by agents.
- **Routing:** the runner writes `result.json` and a `route` event, then saves `state.json` after every step, atomically (tmp + rename).

## How serve supervises a loop

Phase 1 (this contract) runs in the foreground only.

- **Process.** `bough loop run` builds its own `serve.NewSupervisor(Options{Exe, HistDir: sessionsDir(), MetaPath: <runDir>/meta.json, Home})`. It does not touch `~/.bough/serve/meta.json` and does not need a running serve.
- **Web UI.** Children land in `~/.bough/history`, so the web UI lists them (origin `loop`) and serve can adopt them read-only.
- **Coach steering.** Steers go through the runner's own supervisor, which holds the stdin lease. They use the same `Send` stdin-line channel as `POST /api/sessions/{id}/prompt`.
- **Detaching.** `bough loop run --detach` re-execs itself with `setsid` and stdout sent to `<runDir>/runner.log`, then prints the run id. That is enough supervision for now.
- **No new serve API in phase 1.** A later `POST /api/loops` would call `pipeline.NewRunner` with the serve supervisor as `Sessions`. The seam already allows it.

## CLI

```
bough loop run <pipeline.yml> [--detach] [--set id.key=value ...]   exit: 0 passed, 1 failed, 3 exhausted, 130 stopped, 2 invalid
bough loop status [<run-id>]      no id: table of runs (id, name, status, node, step, age); id: state + last 10 events
bough loop stop <run-id>          SIGINT to state.pid; waits up to 10 s for status != running
```

- `--set` values are forwarded to every child.
- Status also reports stale runs: if the status is `running` but the pid is dead, it prints `running (dead)`.

## Holdout: what is enforced

**Enforced (the container boundary):**

1. **Staging.** `stageHoldout` copies the matched files to `<runDir>/holdout/` (mode 0600). `{{holdout_dir}}` points only at that copy.
2. **Local nodes must hold the holdout.** A local node reads the whole host, so every agent node without `holdout` must be `mode: project`, or `Validate` fails. A pipeline without holdout files has no such restriction.
3. **Preflight.** `preflightHoldout` fails if:
   - any staged file's original path lies under a project's repo checkout, i.e. is tracked by `git -C <repo> ls-files --error-unmatch`, for each repo in `~/.bough/projects/<slug>` of the pipeline's project nodes;
   - any holdout file lies under `~/.bough/orbs`, `~/.bough/scratch` or `~/.bough/projects`.
4. **`view` confinement** (`plugins/tools/tools.go`). In project mode, `Stats.view` calls `s.project.allowed("view", path)`. That allows the orb root, `BOUGH_SCRATCH` and the project dir (already the write roots), and it resolves `filepath.EvalSymlinks` before the prefix check. This closes the only host-side read path from a project session. `write` and `patch` get the same `EvalSymlinks` treatment.
   - Guest bash sees only the orb mounts, and the run dir is never mounted.

**Filtered (deterministic, not a boundary):**

5. **Leak filter.** Before a holdout node's reply becomes `{{input}}` for any other node, `leaks` checks it for any 8-word shingle (lower-cased, punctuation stripped) shared with a holdout file. On a hit:
   - `routed.md` becomes `"[validator feedback withheld: it quoted the acceptance criteria]"`.
   - A `leak` event is written.
   - The route itself is unchanged.
   - The full reply stays in `reply.md` in the run dir, which project nodes cannot reach.

**Not enforced (stated limits):**

- The relay `bough mcp` runs on the host. A project coder with a file-reading MCP server can read the holdout. Phase 1 docs say: do not configure such MCP servers for the coder's project.
- If the orb proxy can reach host loopback, serve's unauthenticated API on :7683 can expose the validator's transcript. Verify `internal/orb/proxy.go`. Until it refuses loopback targets, treat this as open.
- Paraphrase gets past the shingle filter.
- The user's own local sessions can read `~/.bough/loops`.

## Coach

`coach.go` defines `func (r *Runner) coach(ctx context.Context, c Coach, target func() (id string, running bool))`.

- **Lifecycle.**
  - One goroutine per coach, started with the run.
  - It is active only while the target node's visit is in flight. Between visits it idles.
  - Its ctx is cancelled at run end.
  - Errors are written as `coach` events and never change routing or status.
- **Each tick (`every`):**
  1. Skip if any of these hold:
     - the target is not live;
     - `PendingAsk != nil`;
     - the cooldown since the last steer has not elapsed;
     - `max_steers` is reached;
     - no new entries have arrived since the last tick's seq.
  2. `tail := serve.Transcript(Entries(target), lastSeq, 40)`. Render it as `kind: text` lines, capped at 8k chars.
  3. Run a fresh coach session in the target's mode and project: `Create({ID, Mode: target.Mode, Slug: target.Project, Args: sets+model, Origin:"loop"})`, then `Send` `c.Prompt + "\n\nGoal: " + goal + "\n\nRecent activity:\n" + tail`, then wait for done and take the reply. Kill the session afterwards.
  4. If the trimmed reply is `NONE`, empty, or longer than 500 runes, skip. Otherwise re-check that the target is live, has no pending ask, and that its visit is still the same one; then `Send(target, "[coach] "+reply)`.
     - Mid-turn this steers.
     - If the turn ended in the gap, the line would start a new turn. The runner detects that the visit has ended and skips instead, which narrows the race to a few ms. That remaining race is accepted.
  5. Write `coach/<n>.md` and a `steer` event.
- **Isolation.** The coach prompt holds only the target transcript, never `holdout` or `{{input}}` from holdout nodes. The coach session runs in its target's mode and project, so it can read exactly what the coder can: with holdout files the target is a project node, and so is the coach. A reply that trips the leak filter is still skipped, not sent.

## Tests (temp HOME; never :7683 or real ~/.bough)

- **`internal/pipeline` unit tests.**
  - Build a fake `Sessions` in the test: a map of scripted replies per node and visit, emitting `done`/`exit`.
  - Load and validate: bad targets, missing `max_visits`, local node without holdout while holdout exists, and verdict set on a check.
  - Routing:
    - check pass and fail use `sh -c 'exit 0'` / `'exit 1'`;
    - verdict parsing covers PASS, FAIL, a missing line (FAIL), and trailing whitespace;
    - `max_visits` produces `exhausted`;
    - ctx cancel produces `stopped` and kills the child.
  - Run dir: every step has prompt/reply/result, `state.json` round-trips, and events stay in order.
  - Leak filter: an 8-word quote is withheld, a paraphrase passes, and short common phrases pass.
  - Coach (fake clock or short intervals):
    - steers once and then respects the cooldown;
    - skips on `NONE`, when there is a pending ask, and when not live;
    - never changes the route.
- **`internal/serve`.** `Create` with a pre-minted `ID` uses `BOUGH_FAKE_NEWID`, finds the session without the dir diff, and passes `Args` through to argv. It reuses `newFixture`.
- **`plugins/tools`.** Project-mode `view` refuses a path outside the roots and a symlink inside the worktree that points outside.
- **`e2e/loop_test.go`** (real binary, `runCLIEnv`, `--set llm.plugin=llm-echo`):
  - A local-only pipeline, agent echo followed by a check `test -f marker` that fails and then passes, where the check itself creates the marker on its second visit. It asserts exit codes 1 and 0, the run dir files, and `bough loop status` output.
  - A verdict node driven by a replay tape that ends in `VERDICT: FAIL`, then a second tape that ends in `VERDICT: PASS`.
  - Project-mode e2e is skipped, because orbs are not faked in e2e. Project coverage stays in the unit tests.
- **Gate:** `go test ./internal/pipeline/... ./internal/serve/... ./plugins/tools/... ./e2e/... 2>&1 | grep -E '^(FAIL|---)'`

## Area A file list

```
go/docs/loops.md
go/internal/pipeline/pipeline.go
go/internal/pipeline/run.go
go/internal/pipeline/agent.go
go/internal/pipeline/check.go
go/internal/pipeline/coach.go
go/internal/pipeline/holdout.go
go/internal/pipeline/pipeline_test.go
go/internal/pipeline/run_test.go
go/internal/pipeline/coach_test.go
go/internal/pipeline/holdout_test.go
go/internal/serve/supervisor.go
go/internal/serve/children.go          (export LastTurn wrapper)
go/internal/serve/supervisor_test.go
go/cmd/bough/loopcmd.go
go/cmd/bough/main.go
go/plugins/tools/tools.go
go/plugins/tools/tools_test.go
go/e2e/loop_test.go
```

---

# Area B: skills (files only)

These skills ship as plain files under `go/skills/`. They are installed with a
symlink into `~/.bough/skills/<name>` (documented in each SKILL.md). No Go
changes, no embedding and no injection: both are `manual: true` and fire only on
`/name`. Names avoid the `commonWords` list (`plan`, `search`).

## multi-model-plan

```
go/skills/multi-model-plan/SKILL.md
go/skills/multi-model-plan/preflight.sh
```

- **Frontmatter.**
  - `name: multi-model-plan`
  - one-line `description:` with usage `/multi-model-plan <task>`
  - `manual: true`
- **`preflight.sh <plugin/model>...`.**
  - For each model, it runs `printf 'Reply with exactly: OK' | BOUGH_WEB_ADDR=127.0.0.1:0 "${BOUGH_BIN:-bough}" --headless --set llm.plugin=P --set llm.model=M`, with a 120 s timeout.
  - A model works if the exit code is 0 and the output contains `OK`.
  - It prints the working models and exits 1 if fewer than 3 distinct ones work.
  - The skill requires 3 candidates from different providers. The default candidates come from `bough.yml` plus `"${BOUGH_BIN:-bough}" rows`, and the user can name others.
- **Procedure in SKILL.md.**
  1. Preflight. If it halts, report which models failed and stop. The skill never falls back to fewer models.
  2. Drafts: run three fresh `bough --headless --set …` processes (shell, in parallel via background jobs), one per model, with the same task brief. Write `plans/<slug>/draft-<model>.md` in the scratchpad.
  3. Critiques: three more fresh runs. Each model critiques only the two drafts it did not write, into `critique-<model>.md`.
  4. Interview: 2-4 `tools.ask` questions, each derived from a disagreement between drafts. Each question has options.
  5. Merge: the current session writes `plan.md` plus `merge-notes.md` (every decision, the source draft, and the critique or answer it rests on).
- **Gate.** Each of the 3 drafts and 6 critique pairings exists and is non-empty, verified with `ls` and `wc`.

## folder-index

```
go/skills/folder-index/SKILL.md
go/skills/folder-index/check.sh
go/skills/folder-index/bench.sh
go/skills/folder-index/evals/README.md
go/skills/folder-index/evals/example.yml
```

- **Frontmatter.**
  - `name: folder-index`
  - `description:` with `/folder-index build|refresh|query <dir>`
  - `manual: true`
- **Layout it writes:** `<dir>/.index/`
  - `README.md`: routing root, what lives here, links to topics
  - `topics/<topic>.md`: links to themes
  - `themes/<theme>.md`: links to leaves
  - `leaves/<leaf>.md`: facts, each citing `path:line`
  - `manifest.tsv`: `path	sha256	leaf` for refresh
- **build.** Read the tree, cluster into topics and themes, and write leaves with citations.
- **refresh.** Diff `manifest.tsv` against `shasum -a 256`, then rewrite only the affected leaves and the parents that route to them.
- **query.** Walk README → topic → theme → leaf, open only the cited lines, and answer with the citations. The index is read on demand only; it is never injected or referenced from AGENTS.md.
- **`check.sh <dir>`.** Exits non-zero if any of these hold:
  - a link is broken;
  - a `path:line` citation points past EOF or at a missing file;
  - a manifest hash is stale;
  - a leaf is orphaned.
- **`evals/example.yml`.** A list of `{q, expect_path}` entries.
- **`bench.sh <dir> <evals.yml> <plugin/model>`.**
  - For each question, it runs two headless sessions: one with the prompt "use `<dir>/.index` via /folder-index query", and a baseline with no index.
  - It records whether `expect_path` appears in the reply, the wall time, and the token usage from the `done` usage entry.
  - It prints a TSV summary.
  - Uses `BOUGH_WEB_ADDR=127.0.0.1:0`.

## Area B file list

```
go/skills/multi-model-plan/SKILL.md
go/skills/multi-model-plan/preflight.sh
go/skills/folder-index/SKILL.md
go/skills/folder-index/check.sh
go/skills/folder-index/bench.sh
go/skills/folder-index/evals/README.md
go/skills/folder-index/evals/example.yml
```
