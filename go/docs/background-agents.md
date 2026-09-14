# Background agents — implementation contract

Status: contract for the bg-agents-wf1 run. Three areas (foundation, spawn,
web-ui) build against this file in parallel. Identifiers named here are used
verbatim; where this file is silent, decide inside your own area and never
edit another area's files (section 8). Builds on `docs/orbs.md`
(local = host read-only, project = orb that writes).

## 0. The model in one screen

- A background agent is a **separate session** created by `bough serve` via
  its JSON API, not a goroutine in the parent. The parent gets a session id
  back at once and keeps working.
- The child's first `meta` history entry records `spawned_by: <parent id>`.
  The supervisor also passes `BOUGH_SPAWNED_BY=<parent id>` in the child's
  env. A session with `spawned_by` cannot background-spawn (depth 1), and
  the supervisor refuses it too.
- When a child's turn ends, the supervisor calls
  `Supervisor.Notify(parent, "[agent <title> · <id> finished] <reply…>")`
  exactly once per child turn. A live parent gets a stdin line
  `{"notice": "..."}`; a stopped parent gets a `notice` history entry that
  its loop delivers on next mount. A report never starts a stopped parent.
- Background shell jobs become typed `job` entries so serve reads them
  without regexes.
- Nothing about queued children survives a serve restart (out of scope).

## 1. History entries, Go identifiers, service keys

### 1a. `job` entries (foundation)

Existing: the loop's `landJobs` / wake path writes `kind:"job"` with
`data.text` = the notice text (`job N [exited 0] cmd (3s)\n<tail>`). That
stays unchanged — it is what the model and the transcript read.

New: `plugins/tools` appends **typed** job entries through the `history`
service. The tools row today reaches history only through
`kernel.Get[interface{ Path() string }](ctx, "history")`. It does NOT
import `plugins/history`, and must not start: an interface naming
`Append(...) history.Entry` would need that import (plugin imports plugin).
Instead the history row additionally provides

```go
// plugins/history Apply, next to ctx.Provide("history", s)
ctx.Provide("history-record", func(kind string, data map[string]any) { s.Append(kind, data) })
```

and tools resolves `kernel.Get[func(string, map[string]any)](ctx,
"history-record")` in the same place it resolves `history` (same provider
row, so no new remount edge). Optional: without it no typed entries are
written.

Entry shape, kind `"job"`, discriminated by the presence of `event`:

| field   | type   | started | finished | meaning |
|---------|--------|---------|----------|---------|
| `id`    | int    | yes     | yes      | job id (restarts with the process) |
| `event` | string | `"started"` | `"finished"` | |
| `cmd`   | string | yes     | yes      | full command, first line only |
| `until` | string | if set  | if set   | watch regexp source, else omitted |
| `exit`  | int    | —       | yes      | exit code; `-1` for killed/timeout/start error |

Written in `Jobs.start` right after `c.Start()` succeeds (started) and in
the Wait goroutine before `j.notify` (finished). Append goes through a
func field `Jobs.record func(kind string, data map[string]any)` set by
`Apply` so jobs.go stays testable without history. The existing text
return of `tools.bash` ("job N started in the background (limit …): cmd")
is unchanged — the model reads it.

Must NOT be confused with notice `job` entries: every consumer that reads
`job` entries (serve `Transcript`, web `JobBlock`, the loop projection)
skips entries whose `data.event` is a string. Foundation owns the Go
skips (`internal/serve/status.go` Transcript, `plugins/loop` projection if
it reads `job`); web-ui owns nothing here because Transcript never sends
them.

`internal/serve/signals.go` `RunningJobs`: first pass over typed entries
(`event` present): started opens `{ID, Cmd, Started: e.At}`, finished
closes. If ANY typed entry exists, the regex path is skipped entirely; else
the existing `jobStarted`/`jobFinished` regexes run (old files). Keep the
regexes, comment them as legacy fallback.

### 1b. `notice` entries (foundation)

Kind `"notice"`, data:

```json
{"id": "<history.NewID()>", "to": "<target session id>", "text": "<notice text>", "from": "<child id or \"\">"}
```

`to` exists because `history.Fork` copies ancestor entries: a fork of a
parent would otherwise re-deliver every notice still undelivered at the
fork point. Delivery only considers notices whose `to` equals the mounting
session's own id (basename of `r.hist.Path()`-equivalent; the loop gets it
from the `history` service's `Path()` via a local optional assertion).

Delivered marker, kind `"notice-delivered"`, data `{"id": "<notice id>"}`.

Writer when the target session has no live child: the supervisor, through a
new exported history helper (foundation, `plugins/history`):

```go
// AppendFile appends one entry to a session file that serve does not hold
// (it MAY still be held by a TUI `bough --resume` serve cannot see),
// chaining Seq/Parent from the file's last line under the same lockFile
// flock Store.Append takes, so a holder's catchUp skips past its seq and
// no line is torn.
func AppendFile(path, kind string, data map[string]any) (Entry, error)

// ReadFile returns every line of a session file, all branches, in file
// order (exported wrapper over readEntries).
func ReadFile(path string) ([]Entry, error)
```

Delivery on mount (foundation, `plugins/loop`): after the runner resolves
`r.hist` and `r.notices`, and before the idle `wake` select starts, read
`history.ReadFile(path)` — the raw file, NOT `r.hist.Entries()`: a notice
appended while a TUI held the file sits on a side branch (its Parent is the
holder's last line at that moment, later holder lines chain past it) and a
branch walk would never see it. Take `notice` entries with `to` == own id
and no `notice-delivered` for that `id` anywhere in the file; for each, in
seq order: `r.hist.Append("notice-delivered", {"id"})` FIRST, then
`Notify(text)`. Skip the whole scan (mark nothing) when the job-notices
service lacks `Notify` — marking without a queue would lose the notice.
Marking before queuing makes a crash between the two lose the notice rather
than deliver it twice — exactly-once is "at most once plus the common path".
A TUI-held parent gets serve's notices at its next mount, not live (serve
has no pipe to it); that is the spec's "when that session next mounts".
The existing `Wake()` already wakes a loop that mounts with pending
notices. Neither kind reaches the model's projection directly; the text
arrives as the normal `job` note the wake/landJobs path writes.

Loop needs `Notify(string)` on the seam: extend the loop's `Notices`
interface locally with an optional `interface{ Notify(string) }` assertion
(Jobs already has it). No new service key.

### 1c. `meta.spawned_by` (foundation writes the field, spawn sets the env)

The env must NOT stay in the process: the child's `tools.bash` would pass
`BOUGH_SPAWNED_BY`/`BOUGH_SESSION_ID` to any `bough -p` it runs, which
would then nest under the wrong parent, eat a per-session slot, or try to
create the child's own history file. So, exactly like `takeModeEnv`
(cmd/bough/mode.go): `cmd/bough` reads AND clears both vars before mount
and provides them as plain values `ctx.Provide("session-spawned-by", v)`
and `ctx.Provide("session-id", v)` (next to `session-mode`, main.go).
`plugins/history` Apply reads them with `kernel.Get[string]` (as
`sessionMode` does): `session-id` non-empty ⇒ the fresh file is
`<session-id>.jsonl` instead of `NewID()`; `session-spawned-by` non-empty
⇒ `data["spawned_by"]` on the created meta. `SessionInfo` gains
`SpawnedBy string` read from meta. `Classify` is unchanged: children are
`origin` web, visible, nested by UI.

Depth check in the child (workers): `session-spawned-by` value (fresh) OR
the `spawned_by` of the `meta` entry in the `history` service's
`Entries()` (a resumed child is started by `ensure` without the env).
workers already has a local `Entries()`-bearing history seam
(workers.go:133). A user `Fork` of a child writes a new meta without
`spawned_by`, so the fork is a person's session and may spawn — intended.

### 1d. Headless notice line (foundation)

`plugins/ui/headless.go` `headlessPump`: the `{`-prefixed JSON parse grows

```go
var obj struct {
	Prompt string `json:"prompt"`
	Notice string `json:"notice"`
}
```

If `obj.Notice != ""`: call `hlNotify(obj.Notice)` and `continue` —
**before** `hlAnswerPending` (which clears `hlAsk` as its first act, so
even a failed parse must not fall through to it: a line that unmarshals
with a non-empty `notice` never reaches the ask, the `/` dispatch, steer
or submit). Check `Notice` before `Prompt`. `hlNotify` is a func set in
`runHeadless` from a new argument `notify func(string)` that `ui.go`
builds as a closure resolving `kernel.Get[interface{ Notify(string) }](ctx,
"job-notices")` AT CALL TIME, not at mount: the tools row may mount after
ui, and a mount-time Get would also make every tools remount remount the
headless ui row. With
no service: write an `error` hlLine `"ui: headless: notice dropped: no
job-notices service"`. A notice never counts toward `hlPending` (it is not
a prompt with a `done` owed), so EOF drain does not wait on it.

### 1e. `Supervisor.Notify` (foundation)

```go
// internal/serve
func (s *Supervisor) Notify(id, text string) error
```

- child in `s.kids[id]` (live): `write` the raw JSON line
  `{"notice": text}` (bypass `write`'s prompt wrapping: add
  `writeLine(ch, v any)`), and `emitLocked(id, "notice", text, nil)` so
  the browser sees it. Does NOT go through `Send` (which refuses on a
  pending ask and calls `ensure`). If the write fails (child exited
  between the lookup and the write), fall through to the no-child path.
- no child: `history.AppendFile(<HistDir>/<id>.jsonl, "notice", {id,to:id,text,from})`.
  Never calls `ensure`. Archived targets still get the entry.
- unknown id (no file): `ErrUnknownSession`.
- `from` comes from a variant `notifyFrom(id, from, text string)` that
  Notify(id,text) calls with `""`; spawn's report calls notifyFrom.

### 1f. Service keys

| key | provider | consumers | change |
|---|---|---|---|
| `job-notices` | plugins/tools | loop, ui (headless, new, resolved per call) | consumer added |
| `history-record` | plugins/history | tools | **new** |
| `session-spawned-by` | cmd/bough (main.go) | history, workers | **new** (plain string) |
| `session-id` | cmd/bough (main.go) | history | **new** (plain string) |
| `spawn-background` | plugins/workers | — | **deleted** |

The new keys are plain values from rows that already feed their consumers:
no new dependency edge between plugin rows, so no remount loop. README table: add `ui` to job-notices consumers
(foundation edits the table row; spawn adds the prose section, see 8).

## 2. Workers → serve

New package `go/internal/serveclient` (spawn). Pure net/http.

```go
package serveclient

// Addr reads $HOME/.bough/serve.pid ("<pid> <addr>\t<cwd>\t<config>\t<caps>"),
// checks the pid is alive, and returns "http://<addr>". ErrNoServe when
// the file is missing, unparsable or names a dead pid. It never removes
// the file (that is cmd/bough's job).
func Addr(home string) (string, error)
var ErrNoServe = errors.New("serveclient: no running bough serve")

type Client struct{ Base string; HTTP *http.Client }
func (c *Client) CreateChild(ctx context.Context, req ChildRequest) (ChildResponse, error)
func (c *Client) Agent(ctx context.Context, id string) (AgentState, error)
func (c *Client) Interrupt(ctx context.Context, id string) error
```

Pidfile parsing: move `parsePidfile` and `alive` out of
`cmd/bough/update.go` into `go/internal/servepid` (`Parse(s string) (pid
int, addr, dir, config, caps string, err error)`, `Alive(pid int) bool`);
cmd/bough calls them. A plugin never imports cmd/bough. Unix `alive`
(signal 0) and Windows variants move with it if split by build tag.

Tests point `Addr` at a `t.TempDir()` HOME whose serve.pid names
`os.Getpid()` and an `httptest.Server` address.

## 3. Supervisor: spawn, limits, queue, report (spawn)

### 3a. JS surface (plugins/workers)

`tools.spawn(task, opts)` — when the second argument is an object with
`background: true`. (Today the second arg is a schema map; disambiguate by
the `background` key: an options object is `{background, project}` only; a
schema combined with background is refused: `"workers: a background agent
reports text; drop the schema"`.)

Returns immediately:

```js
{session: "<child id>", status: "running" | "queued"}
```

Errors (thrown, exact text):

| condition | message |
|---|---|
| empty task | `workers: spawn needs a non-empty task` (existing) |
| caller has spawned_by | `workers: a background agent cannot start agents (depth 1)` |
| no serve | `background agents need bough serve (start it with ` + "`bough serve`" + `)` |
| per-session cap | `workers: background agent limit reached (<n> per session) — do the remaining work yourself` |
| unknown project | `workers: unknown project "<name>"` (from API 400) |
| other API error | `workers: background spawn: <api error>` |

Background spawns do not touch `spawns`/`maxSpawns`/`inChild`.

`tools.agent(id)` →
`{status, title, reply, project}` where `status` ∈ `"queued" | "running" |
"waiting" | "done" | "failed" | "stopped"` (serve `Status` string, plus
`queued`), `reply` = full text of the child's last `assistant` entry
(there is no `reply` kind; `""` none), `project` = slug or `""`. Unknown: `workers: no agent "<id>"`.
Only the caller's own children are visible (API checks `spawnedBy`):
`workers: agent "<id>" was not started by this session`.

`tools.stopAgent(id)` → `"stopped"`; queued → removed from queue,
`"stopped"`; already idle → `"not running"`. Same ownership errors.

Describe lines + prompt section update mention both.

Parent session id: `kernel.Get[interface{ Path() string }](kctx, "history")`
→ basename without `.jsonl`. Parent mode: `kernel.Get[string](kctx,
"session-mode")` and `"session-project"`.

Config (workers row, documented in README as the agents settings):

```yaml
- id: workers
  plugin: workers
  config:
    max_per_session: 200   # background agents started by one session, lifetime
    max_running: 16        # children running at once across serve; more queue
```

The child sends both in the create request; serve applies the request's
`maxPerSession` to that parent and keeps the LAST seen `maxRunning` as the
global cap (default 16 when a request omits it). Unknown-key validation in
Apply grows the two names.

### 3b. API: create child

`POST /api/sessions` body grows (all optional; existing callers unchanged):

```json
{"cwd": "...", "prompt": "<task>", "mode": "local|project", "project": "<label id>",
 "slug": "<project slug>", "spawnedBy": "<parent id>",
 "maxPerSession": 200, "maxRunning": 16}
```

With `spawnedBy`: `project` resolution — workers sends `slug` (a project
definition name); the handler validates `projectdef.ValidSlug` + existence,
else 400 `serve: api: unknown project "<name>"`. Mode: `slug` set ⇒
project; else the parent's mode (and slug) from its meta; local ⇒ `cwd` =
the parent's cwd. The handler rejects: parent unknown 404; parent has
`spawned_by` 409 `serve: api: session "<id>" is itself a background agent
(depth 1)`; count ≥ maxPerSession 429
`serve: api: background agent limit reached (<n> per session)`.

Response 201: the usual row plus `"queued": true|false`. A queued child has
no id from a process yet, so the supervisor mints it: see 3c.

### 3c. Supervisor queue and counts

```go
// CreateOptions ALREADY exists (supervisor.go: Cwd, Prompt, Mode, Slug);
// spawn adds one field, it does not redeclare the type.
	SpawnedBy string // "" = a person's session

func (s *Supervisor) CreateChild(opt CreateOptions, maxPerSession, maxRunning int) (id string, queued bool, err error)
func (s *Supervisor) Children(parent string) []ChildInfo
func (s *Supervisor) StopChild(id string) error // interrupt live, drop queued

type ChildInfo struct {
	ID, Parent, Title, Project string
	Queued bool
	Status Status // derived; "" when queued
}
```

- Session id minting: children need an id before a process exists (queued).
  The supervisor mints `history.NewID()` and passes `BOUGH_SESSION_ID=<id>`
  so the child's history row creates exactly that file (foundation: 1c,
  cmd/bough takes the env, history reads `session-id`; spawn adds it to
  `start` extra env). Create's directory diff is skipped for this path.
- `StatusQueued Status = "queued"` is declared in `children.go` (spawn),
  not in foundation's `status.go`: it is never derived from history.
- `SessionMeta` grows `SpawnedBy string` (persisted in meta.json, so the tree
  survives a serve restart for sessions that ran) and `Queued bool`
  (in-memory only; never persisted — queued children are lost on restart,
  and meta.json load drops entries that have neither a history file nor
  another field set).
- Queue: `s.queue []queuedChild` FIFO in memory, guarded by `s.mu`.
  "Running" = children with `SpawnedBy != ""` that were started (process
  spawned, task written) and whose current turn has not closed with a
  `done`/`cancelled` entry — counted from the moment of start, NOT from
  the first derived `running` status, or a burst of spawns all see 0
  running while their processes boot and overrun the cap. Idle live
  children do not hold a slot. Over the cap is never an error: queue. When a slot frees (a child `done`/`cancelled`/`error`
  /`exit` event) the supervisor pops the head and starts it. Starting a
  queued child = spawn with the minted id + write the task as first prompt.
- Per-session count: number of `SessionMeta` with `SpawnedBy == parent`
  (queued included, archived included) — lifetime, not concurrency.
- Depth: `CreateChild` returns `ErrDepth` when the parent's SessionMeta or
  meta entry has spawned_by. Env `BOUGH_SPAWNED_BY` is set for every child.

### 3d. Report

- Turn end = a `done` or `cancelled` event from a child with
  `SpawnedBy != ""`. The loop does NOT emit `done` on every path: esc/
  interrupt ends with `finish("cancelled")`, and `error` is a MID-turn note
  (loop.go 1567/1625) — reporting on `error` events would report several
  times per turn. Word: `finished` for done with no `error` entry in the
  turn, `failed` for done with one, `stopped` for cancelled. A child
  process exiting with an open turn (`exit` event, no closing entry)
  reports `stopped` once, keyed on that turn's `input` seq.
- The event is only a trigger. The report reads `s.Entries(id)` and uses
  the closing `done`/`cancelled` ENTRY; if the file does not yet show it
  (stdout event raced the flush), retry every `createPoll` up to 2s before
  reporting from what is there. The closing entry is appended after the
  turn's final `assistant` note, so seeing it guarantees the reply is on
  disk before the parent is told.
- Reply = `text` of the last `assistant` entry between the turn's `input`
  and its closing entry (no `reply` kind exists); truncated to **2000
  runes** with `…` and the suffix `\n(tools.agent("<id>") has the full reply)`.
- Text: `[agent <title> · <id> finished] <reply>`; title falls back to the
  id tail (last 6).
- Exactly once per turn: `s.reported map[string]int64` = seq of the last
  closing (`done`/`cancelled`, or `input` for the exit case) entry reported
  per child; skip when ≤. Set under `s.mu` BEFORE calling `notifyFrom`. Kept in memory;
  on serve restart a child mid-turn reports at its next done (acceptable).
- Delivery: `notifyFrom(parent, child, text)` (1e). Never `ensure`.

### 3e. API additions

| method + path | body / query | response |
|---|---|---|
| `POST /api/sessions` | +`slug`,`spawnedBy`,`maxPerSession`,`maxRunning` | row + `queued` |
| `GET /api/sessions/{id}/children` | — | `{"children": [Row…]}` (queued rows included, `status:"queued"`) |
| `GET /api/sessions/{id}/agent` | `?parent=<id>` | `{"status","title","reply","project","spawnedBy"}` ; 403 if parent mismatches |
| `POST /api/sessions/{id}/stop` | `{"parent": "<id>"}` optional | `{"ok":true,"was":"running|queued|idle"}` |
| `POST /api/sessions/{id}/notify` | `{"text": "..."}` | `{"ok":true}` (debug/acceptance) |
| `POST /api/sessions/{id}/archive` | +`{"stopChildren": true}` optional | unchanged; with the flag, StopChild every running/queued child first |

`Row` (Go + TS) gains: `spawnedBy?: string`, `queued?: boolean`,
`agents?: {running: number, queued: number, total: number}` (omitted when
total is 0). `GET /api/sessions` includes queued children as rows
(`status: "queued"`, `entries: 0`). `Status` gains `"queued"`.

## 4. UI (web-ui)

Types (`src/types.ts`): `Status` adds `"queued"`; `Row` adds the three
fields above; `api.ts` adds `children(id)`, `stopAgent(id)`,
`archive(id, {stopChildren})` (keeps `archive(id)` working).

Surfaces, all additions to existing components in `src/app.tsx`:

1. **Sidebar tree nesting.** In `Sidebar`, before `byWorkspace`, pull rows
   with `spawnedBy` whose parent is present out of the section lists; render
   them right after the parent's `.session` block inside a
   `<div className="session-kids" role="group">` using the SAME `session()`
   row renderer (status mark via `StatusMark`, `ModeChip`). A child whose
   parent is absent (archived/hidden) renders top-level as today. Queued
   uses `StatusMark status="queued"` — add `queued` to `STATUS` in
   `status.tsx` (glyph: hollow clock, colour `--text-3`).
2. **Parent head count.** In `Thread` `.thread-head .head-main`, after
   `ModeChip`: `<AgentsChip row onOpen>` — `button.status.mono.head-agents`
   "`N agents`" (running+queued; hidden at 0), opens a popover reusing the
   `.head-pop` / `.head-pop-item` family listing children (title, status
   mark, mode chip); click selects the child.
3. **Child head backlink.** Same place: when `row.spawnedBy`,
   `<span className="meta-line head-parent">spawned by <button
   className="link">{parent title}</button></span>`; parent title from the
   loaded rows, id tail when not loaded.
4. **Archive confirm.** `onArchive` in App: if the row has
   `agents.running + agents.queued > 0`, ask "Stop its N running agents
   too?" with actions "Stop and archive" / "Archive only" / Cancel. The
   existing `askConfirm` (dialog.tsx) is two-way (`Promise<boolean>`), so
   web-ui adds `askChoice(title, body, actions: string[]):
   Promise<string | null>` next to it in `dialog.tsx`, same `DialogHost`,
   and keeps `askConfirm` unchanged.

New CSS (in `dist/index.html` `<style>`, then `bun run design:sync`):
`.session-kids` (left indent = row mark width via existing spacing, a
`--line` left rule), `.head-agents`, `.head-parent`. Tokens only. Phone
<720px: `.session-kids` indent halves; head chips wrap with `.head-main`.

Stories: `src/stories/agents.stories.tsx` (parent+children sidebar, queued
child, parent head with count, child head backlink, archive confirm);
`ds-entry.ts` exports `AgentsChip`. Fixtures in `stories/fixtures.ts`.
Update `.design-sync/conventions.md` class-family table (Rows: `.session-kids`;
Shell: `.head-agents`, `.head-parent`).

## 5. Removing `spawn-background` (spawn)

Confirmed callers: only `plugins/workers/workers.go:722` (provide) and
`plugins/workers/subagents_failures_test.go:295` (test). Delete
`Workers.Background`, the `bg` field and both `w.bg > 0` checks in
`spawn`/`spawnAll`, fold `runChildTo`'s `sink` back into `runChild` (only
Background passed a sink), delete the test case. No README row exists for
the key.

## 6. Exactly-once summary

| message | guard |
|---|---|
| live notice | one stdin line; child queues once |
| stopped notice | `notice` entry + `notice-delivered` marked before queuing; raw-file scan |
| forked parent | notice `to` ≠ fork id ⇒ never delivered in the fork |
| TUI-held parent | entry lands via flock; delivered at its next mount |
| child report | `reported[child]` = last reported closing-entry seq, set before notify |
| serve restart | queued children and the `reported` map are lost; no replay of old events ⇒ no duplicate (a done while serve was down is never reported — out of scope) |
| queued start | popped under `s.mu`, removed before spawn |

## 7. Test plan

Foundation:
- `plugins/tools`: bash with limit writes started then finished typed
  entries (fake history recorder); exit -1 on timeout; no history ⇒ no panic.
- `internal/serve/signals_test`: typed-only, legacy-only, mixed (typed wins),
  Transcript skips typed job entries.
- `plugins/ui/headless_test`: `{"notice"}` with a pending ask does NOT
  answer it and calls notify; no service ⇒ error line; notice at EOF does
  not block drain.
- `plugins/history`: `AppendFile` chains seq/parent, concurrent with an
  OPEN Store appending (race; the Store's next seq skips past it); meta
  records `spawned_by` from `session-spawned-by`, file named by `session-id`.
- `cmd/bough`: both env vars are cleared from `os.Environ()` after take (a
  `tools.bash` child would not see them).
- `plugins/ui` headless: notice resolved per call works when the tools row
  mounts after ui.
- `plugins/tools`: typed entries go through a fake `history-record` func.
- `plugins/loop`: session mounted with two undelivered + one delivered
  notice ⇒ one wake turn with both texts, two `notice-delivered`; remount ⇒
  nothing (llm-echo); a notice on a side branch (appended while another
  Store held the file) is delivered; a `history.Fork` past an undelivered
  notice mounts with nothing delivered; no `Notify` seam ⇒ nothing marked.
- `internal/serve/supervisor_test`: Notify live writes the JSON line (fake
  child script echoing stdin); Notify stopped appends entry and does not
  spawn.

Spawn:
- `internal/servepid`, `internal/serveclient`: parse, dead pid, httptest.
- `plugins/workers`: background spawn against httptest serve returns
  `{session,status}`; no pidfile ⇒ exact error; spawned_by env ⇒ depth
  error; project option forwarded as slug; schema+background refused;
  agent/stopAgent shapes; blocking spawn/spawnAll tests untouched and green.
- `internal/serve`: CreateChild over cap 429; maxRunning=1 queues the second
  (`queued:true`), starts it when the first emits done; three creates in a
  burst with maxRunning=1 start exactly one; depth 409; report
  fires once per done with truncation at 2000 runes, not on mid-turn
  `error` events, `stopped` on cancelled, waits for the closing entry on
  disk (event before flush); stopped parent gets a
  `notice` entry and no process; archive `stopChildren` stops children;
  `GET children`. Fake children via the existing test helper binary / llm-echo.

Web-ui:
- Playwright spec `tests/web/specs/background-agents.spec.ts` against a
  fixture server: nesting, queued mark, head count popover, backlink,
  archive confirm both branches, phone width 400px screenshots.
- `bun run build && bun run check`.

### Live acceptance (isolated HOME, never the user's serve)

1. `HOME=$scratch/home bough serve --run 127.0.0.1:7699` from the worktree
   build (`go build -o $scratch/bough ./cmd/bough`), llm-echo or replay tape.
2. Create a local session in the web UI; its code runs
   `tools.spawn("list go files", {background:true})` ⇒ `{session, status:"running"}`.
3. Sidebar shows the child nested with a running mark; parent head "1 agent".
4. Child finishes ⇒ parent transcript shows `[agent … finished] …` note
   and an unprompted turn; `tools.agent(id).reply` is full.
5. Stop the parent (kill its child process via archive→unarchive), spawn
   report arrives ⇒ `notice` in its file; send parent a prompt ⇒ notice
   delivered once; restart parent again ⇒ not delivered again.
6. `{background:true, project:"<slug>"}` from local parent ⇒ project chip
   child with its orb (skip if no container runtime; say so).
7. Set max_running 1, spawn 3 ⇒ two `queued`, start in order.
8. Archive parent with running children ⇒ confirm dialog; "Stop and archive"
   stops them.

## 8. Area ownership

| file | owner | others need |
|---|---|---|
| go/plugins/tools/tools.go | foundation | — |
| go/plugins/tools/jobs.go | foundation | — |
| go/plugins/tools/*_test.go (new job-entry tests) | foundation | — |
| go/plugins/ui/headless.go, ui.go, headless_test.go | foundation | — |
| go/plugins/loop/loop.go (+tests) | foundation | — |
| go/plugins/history/history.go, tree.go (+tests) | foundation | spawn: `session-spawned-by`, `session-id`, `history-record`, `ReadFile`, `AppendFile`, `SessionInfo.SpawnedBy` (foundation implements) |
| go/cmd/bough/main.go, mode.go (env take + Provide) | foundation | spawn sets the env in `start` |
| go/internal/serve/signals.go, status.go (+tests) | foundation | web-ui: none |
| go/internal/serve/supervisor.go | **spawn** | foundation: `Notify`, `notifyFrom`, `writeLine` — foundation writes them in a NEW file `go/internal/serve/notify.go` (+`notify_test.go`), touching supervisor.go not at all |
| go/internal/serve/notify.go, notify_test.go | foundation | spawn calls `notifyFrom` |
| go/internal/serve/api.go (+api tests) | spawn | web-ui consumes shapes in 3e |
| go/internal/serve/children.go, children_test.go (queue, report, CreateChild) | spawn | — |
| go/internal/servepid/*, go/internal/serveclient/* | spawn | — |
| go/cmd/bough/update.go, serve.go (use servepid) | spawn | — |
| go/plugins/workers/workers.go, subagents_failures_test.go, new bg tests | spawn | — |
| go/bough.yml (workers config comment) | spawn | — |
| go/README.md | spawn | foundation: job-notices consumer `ui`, notice/job entry kinds — send text to spawn; spawn applies |
| go/docs/background-agents.md | architect | amendments only via architect |
| go/internal/serve/web/src/types.ts, api.ts, app.tsx, status.tsx, dialog.tsx | web-ui | — |
| go/internal/serve/web/src/stories/agents.stories.tsx, fixtures.ts, ds-entry.ts | web-ui | — |
| go/internal/serve/web/dist/* (rebuild), design/bough.css (via design:sync) | web-ui | — |
| go/tests/web/specs/background-agents.spec.ts | web-ui | — |
| .design-sync/conventions.md | web-ui | — |

Merge order: foundation → spawn → web-ui (web-ui develops against fixtures
first; rebuild dist after rebase, never hand-merge app.js).
