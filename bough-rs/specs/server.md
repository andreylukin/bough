# Port spec: `server/` — the HTTP surface (routes, SSE, app wiring)

Source: `src/server/*.ts` (16 modules + tests) plus the contracts it rides on:
`src/types.ts` (AppCtx/Db/Bus ports), `src/bus.ts`, `src/schema/events.ts`,
`src/schema/requests.ts`, `src/schema/parts.ts`, `src/errors.ts`.
Target: axum server that the ratatui client is written against. **This file IS the
API contract** — every route, body, response shape and SSE frame is listed in §3.

---

## 1. Purpose & invariants

The server is a **loopback-only JSON API + SSE stream + artifact host**. There is no
web UI (`GET /` returns a plain-text pointer saying so). Invariant comments, verbatim
from the module headers — each one is a porting requirement, not commentary:

- `main.ts`: "**process wiring lives here and only here.** Everything else in the
  tree receives what it needs as a parameter … `app.ts` exports `createHandler(ctx)`,
  and a test builds its own ctx over an in-memory database and never runs this file
  at all." Also: "**Loopback only.** The listener binds `127.0.0.1` with no override,
  because there is no auth layer and none is planned … Binding anywhere else would
  silently publish an unauthenticated API that runs arbitrary programs as the user."
  `BOUGH_PORT` (default 4321) moves the listener; `BOUGH_HOME` relocates the data root.
- `app.ts`: "**HTTP lives here and nowhere else.** A domain module signals failure by
  throwing an `HttpError` subclass carrying its status; the single catch below is
  what turns that into a response." And: "**The route table is APPEND-ONLY** …
  Matching is on the pathname only … in table order, first match wins." And: "**No
  CORS headers, ever.** … Their absence is what stops a webpage the user happens to
  visit from reaching this loopback API and driving the agent … this is the whole of
  its access control."
- `http.ts`: "**nothing a handler module imports may import a handler module back.**"
  (a TS module-initialization-order hazard; in Rust this becomes ordinary module
  layering — keep helpers in a leaf module anyway).
- `events.ts`: "**`seq` is a dedupe key, not a resume cursor.** It is
  process-monotonic and resets on server restart, so there is nothing here that
  replays, buffers, or accepts a cursor. A reconnecting client re-fetches
  `GET /sessions/:id` and reconciles by message id … no frame carries an SSE `id:`
  field." Also: "**a connection that goes away releases its bus subscription,
  always**" (teardown from three triggers: stream cancel, request abort, failed
  write; idempotent). Also: "`?sessionId=` scopes the stream to one session, but
  **an event with no `sessionId` is global and always delivered.**" Filtering does
  NOT resolve lineage — a subagent's events go out under its own session id.
- `bus.ts`: "**one bad subscriber cannot silence the others.**" and "**the bus is
  display transport, never storage.** … Persist first, then publish."
- `sessions.ts`: "**derived visibility** … A session of a COLLAPSING kind
  (`subagent`, `workflow_agent`, `schedule_run`) sits under its `originId` and
  surfaces only on drill-in — because of what it *is*, not because anything marked
  it. There is no archive, deprecate, hide or purge verb." And: "**the thread is
  assembled, never stored.**" And: `startTurn` "arrives on the ctx rather than as an
  import: this module … **does not know how a turn runs**."
- `search.ts`: "**the search index is never load-bearing.** A failure to index must
  never fail the write that triggered it." Search "is allowed to fail quietly; it is
  not allowed to fail invisibly" (swallowed-error counter surfaces on every search
  response until `POST /search/reindex`).
- `jobs.ts`: "**a session's job list covers the work done on its behalf, not just
  the work its own turn started**" (transitive walk over collapsed kinds). Kill is by
  id across sessions; reading output is non-destructive (never advances the model's
  `bashOutput` cursor).
- `turns.ts` (interrupt): it is "**an ANSWER, not an error, that nothing was
  running**" (200 + `interrupted:false`), and "**it does not wait**" — signal and
  return; the client learns the stop from `turn.finished`.
- `artifacts.ts`: "**every served HTML artifact gets the comment layer injected AT
  SERVE TIME**, and nothing else does" (bytes on disk stay what the agent wrote).
  "TRAVERSAL IS A 403, NOT A 404." Browser 404s get an HTML page, clients get JSON.
- `comments.ts`: "**the sidecar lives OUTSIDE the artifact directory**"
  (`~/.bough/comments/<sessionId>.json`, sibling of `~/.bough/artifacts/`). "ONE
  BATCH, ONE TURN." A corrupt sidecar reads as empty.
- `changes.ts`: "**revert never touches a path the session did not change**" —
  enforced by intersecting the request with the live change set. "**a workspace that
  is not a repository degrades, it does not fail.**" No event on revert (closed
  event set); instead a no-wake system note tells the model its edits were reverted
  on purpose.
- `theme.ts`: "**a theme is pure data, and the SERVER owns the token set.**"
  Validation bites on write; read forgives (corrupt file → default palette).
- `skills.ts`: "**the filesystem is the source of truth, and this endpoint reports
  it as it is — including the parts of it that are broken.**" No skill CRUD over HTTP.
- `questions.ts`: holds are memory-only; `GET /questions` is a reconnect path, not a
  feed; settling a settled question is a 409, never a silent success.
- `errors.ts`: "**Error text is a product surface**" — each message names what
  failed, the state that caused it, and the move that resolves it.
- `attachments.ts`: "image bytes cross the loopback boundary once, are checked
  before they touch disk, and thereafter messages carry only the durable path."
- `fs.ts`: "**the candidate list is what git tracks, plus what git would let you
  add**" — `git ls-files`, never a directory walk; non-repo → empty list, not error.
- `models.ts`: catalog is answered by the process that holds the credential
  (server-side discovery); "NEVER SLOWER THAN THE TERMINAL IT BLOCKS" (2.5s deadline
  race against a cached/static answer).
- `defaults.ts` (model.json): "FORGIVING ON READ, like the theme."

---

## 2. Public API (per module)

### `http.ts` — primitives (leaf; no deps beyond errors + Zod)
- `type Handler = (req, ctx: AppCtx, params: Record<String,String>) -> Response|Future<Response>`
  — params hold only the named groups that matched (unmatched optional group is
  ABSENT, not `undefined`).
- `route(method, pathname, handler) -> Route` — `Route { method, pattern: URLPattern, handler }`.
- `json(body, status=200)` — `content-type: application/json; charset=utf-8`.
- `errorResponse(status, message)` — the envelope `{"error": message}` **every client reads**.
- `parseBody(req, zodSchema, fallback=null)` — parse JSON body; unparseable body →
  `fallback` → schema decides; failure throws `BadRequestError("invalid body: " + zodMsg)`.

### `app.ts` — route table + dispatcher
- `routes: Route[]` — the full table (see §3). Append-only; order significant
  (first match wins; e.g. `/saved-workflows` is top-level because `/workflows/saved`
  would be swallowed by `/workflows/:id`).
- `createHandler(ctx, opts?) -> impl Fn(Request) -> Future<Response>`; opts:
  `{routes?, onUnexpectedError?(err, req)}`. Dispatch: match method exactly + pathname
  in table order → call handler in one try/catch: `HttpError` → its status +
  `{error}`; anything else → report via `onUnexpectedError` and 500 `{error}`.
  Fallbacks (not table entries): `GET /` → text/plain `ROOT_POINTER` ("bough server —
  drive it with the `bough` TUI. There is no web UI: this origin is the JSON API,
  the /events SSE stream, and artifact hosting."); path matched by some entries but
  wrong method → **405** with `allow` header and body
  `{error: "<METHOD> not allowed on <path> — try GET, POST"}`; else **404**
  `{error: "no route for <METHOD> <path>"}`. Note: the root pointer check runs AFTER
  the table loop but BEFORE the 405 computation only in the sense that `GET /` wins
  over a 405 when some other method owns `/` (pinned by test).
- `groupsOf(match)` — strips undefined groups.

### `events.ts` — SSE
- `HEARTBEAT_MS = 15_000`; `CONNECTED_FRAME = ": connected\n\n"`;
  `HEARTBEAT_FRAME = ": ping\n\n"`.
- `passesFilter(event, filter) -> bool` — pure: no filter passes everything; event
  with no sessionId passes any filter; else equality.
- `frame(event) -> String` — `event: <type>\ndata: <JSON of the whole stamped envelope>\n\n`.
  Single `data:` line is safe because JSON escapes newlines. Throws on
  unencodable payload → caller SKIPS that event, keeps the connection.
- `createEventsHandler(opts?) -> Handler`; opts `{heartbeatMs? (0 disables), timers?,
  onStreamError?(err, {phase: "serialize"|"enqueue"})}`. `events` = production instance.
- Response headers: `content-type: text/event-stream`,
  `cache-control: no-cache, no-transform`, `connection: keep-alive`.

### `sessions.ts`
- `COLLAPSED_KINDS` (re-export), `isCollapsed(session)`.
- `SessionListItem = Session + {busy: bool, lastTurnStatus?, costUsd?, tokens?}` —
  all DERIVED at read time (busy from turns; cost/tokens from usage rows; `tokens` =
  input+output+reasoning, cache traffic deliberately excluded; both omitted when 0).
- `type TurnStarter = (ctx, session, message) -> ()`; `WithTurnStarter { startTurn? }` —
  the ctx seam the message post reads. Returns nothing awaited; errors contained.
- `WithModelDefaults { modelDefaultsPath? }` — test seam for `~/.bough/model.json`.
- `normalizeWorkspace(raw, home)` — `~` expansion + absolutize (pure).
- Handlers: `listSessions, createSession, getSession, patchSession, postMessage,
  putDraft, getSessionUsageH, getModelSettingsH, putModelSettingsH`.

### `turns.ts`
- `InterruptResult {sessionId, interrupted: bool, message: String}`.
- `WithTurnRegistry { turnRegistry? }` — test seam; default = process-wide registry
  from `turn/queue.ts`. Handler `interruptSession` calls `interruptTurn(sessionId, registry)`.

### `questions.ts`
- `listQuestions` (`GET /questions[?sessionId=]` → bare array of `AskQuestion`,
  oldest first, from the in-memory hold registry in `hostfn/ask.ts`).
- `answerQuestion` — see §3. Race between read and settle → 409.

### `jobs.ts`
- `setJobRegistry(next) -> prev` — module-level seam over the process-wide
  `JobRegistry` (`hostfn/jobs.ts`); jobs are in-memory and die with the process.
- `jobSessionIds(db, sessionId) -> Vec<String>` — BFS over `sessionsByOrigin`,
  following only `isCollapsedKind` children (forks excluded — sibling conversations,
  not delegated work); `seen` set guards against lineage cycles.
- `jobsForTree(db, sessionId)` — flat-map `registry.listJobs(id)` over the walk.
- Handlers: `listJobsH, runShellH, killJobH, jobOutputH`.

### `artifacts.ts`
- `contentTypeFor(path)` — extension→MIME table (html, htm, js, mjs, css, json, svg,
  png, jpg, jpeg, gif, webp, ico, woff, woff2, map, txt, md→text/plain, csv, wasm);
  default `application/octet-stream`. Extensionless files sniff first 64 bytes:
  leading `<` after trimStart → HTML.
- `injectCommentLayer(html)` — splice `commentWidget()` before last `</body>`
  (case-insensitive), else append.
- `NOT_FOUND_PAGE` — self-contained HTML 404 for browsers.
- `serveArtifact(sessionId, name, opts{dir?, accept?}) -> Response` — confinement
  via `hostfn/artifact.ts::resolveArtifactPath` (throw → 403 plain-text
  "forbidden"); HTML gets the widget injected; everything served with
  `cache-control: no-cache`; missing/dir → 404 (HTML page if `accept` includes
  text/html, else `{error: "no artifact <name> for session <id>"}`).
- `listArtifactsH`, `getArtifactH`. `decodeSegments` percent-decodes **per
  segment** (whole-string decode would turn an encoded `%2F` into a real separator
  = traversal primitive); malformed escape decodes to itself.

### `comments.ts`
- Zod shapes (validated HERE, not on the wire): `CommentAnchor {label≤200 def "",
  selector≤400 def "", xf,yf ∈[0,1] def 0.5}`; `ArtifactComment {id, artifact, text≤4000,
  anchor, ts, sent def false}`.
- Store fns (all take `opts{dir?, now?}`): `commentsPath` (confined, single path
  segment), `loadComments` (total: missing/corrupt → `[]`), `addComment` (bad anchor
  → centered default, text truncated to 4000), `deleteComment -> bool`,
  `markSent(ids)` (called only AFTER the system note landed).
- `COMMENTS_NOTE_PREFIX = "[artifact comments]"`; `formatForAgent(comments)` —
  grouped by artifact, numbered, `(near "label")` clause, closing line "Address the
  comments, or reply with questions." Singular/plural forms pinned by tests.
- `commentWidget() -> String` — ~150-line inline CSS+JS widget; interpolates
  NOTHING (derives session id + artifact from `location.pathname` match
  `^/artifacts/([^/]+)/(.+)$`); talks same-origin to `/sessions/:id/comments`.
  Port as a static string constant.
- Handlers: `listCommentsH, postCommentH, sendCommentsH, deleteCommentH`.

### `changes.ts`
- Noise filter (applied to what the rail SHOWS, hence what revert can touch):
  segments `__pycache__, node_modules, .pytest_cache, .mypy_cache`; basenames
  `.DS_Store`; suffixes `.pyc, .pyo`.
- `SessionChangeSet = ChangeSet + {workspace: String|null}`;
  `sessionChanges(db, sessionId)` — no workspace → `{available:false, reason:…,
  base:null, files:[], workspace:null}` (never falls back to the server's own cwd).
- `RevertOutcome {reverted: Vec<String>, skipped: Vec<String>,
  failed: Vec<{path, error}>}`.
- `revertChanges(db, sessionId, paths?)` — **`paths: []` (explicitly empty) is a 400
  refusal; `paths` ABSENT means revert-all**. Requested paths only cosmetically
  normalized (`./x`→`x`, trailing slash); absolute or `..` paths land in `skipped`.
- Handlers: `getChangesH` (always 200), `revertChangesH` (posts a `wake:"never"`
  system note when anything reverted: "The human reverted <paths> … Do not re-apply
  them unless you are asked to." — without it the model re-applies the reverted
  edit, observed live twice).

### `search.ts`
- `DEFAULT_LIMIT=20, MAX_LIMIT=200`.
- `SearchResultHit {messageId, sessionId, title, kind, collapsed, originId?, role,
  snippet, createdAt}`; `IndexHealth {failures, lastError, lastFailureAt}`;
  `SearchResult {query, effectiveQuery, rewritten, scope, limit, count, hits,
  index?: IndexHealth+{degraded:true, repair:String}}`.
- `searchSafeDb(db, opts?) -> Db` — wrapper installed at boot on `ctx.db`:
  `indexMessage` catches, counts, reports; every other method delegates untouched.
  (TS uses a Proxy + bound-method cache to survive `#private` brand checks; in Rust
  this is just a newtype implementing the `Db` trait, delegating all but one method.)
- `indexHealth(db)`, `indexRecoveredMessages(db, recovered) -> usize` (boot,
  best-effort, idempotent), `rebuildIndex(db) -> {messages, sessions}` (counts the
  CORPUS, not FTS rows; heals the health counter; "no such table" → 503 below).
- `quoteQuery(q)` — each whitespace chunk quoted (`"` doubled), joined `" AND "`.
- `SearchIndexUnavailableError` — 503; message includes the SQLite cause and the
  restart-then-reindex recovery sentence. Discriminated by sniffing the error text
  for `no such table: messages_fts | no such module: fts5 | database disk image is malformed`.
- `searchTranscripts(db, query, {sessionId?, limit?})` — limit clamped to [1,200];
  unknown sessionId → 404; FTS syntax error → retried once with `quoteQuery`, and
  `rewritten:true` + `effectiveQuery` reported; hit whose message row is gone is
  dropped (index drift ≠ result); session lookups memoized per response.
- Handlers: `searchH` (empty q → 400 with the syntax hint BEFORE schema validation;
  Zod issues flattened to `path: message` lines), `reindexH`.

### `theme.ts`
- `THEME_TOKENS` (18 fixed tokens: bg, panel, panel2, panel3, panelInset, canvas,
  border, border2, border3, hairline, text, text2, muted, muted2, green, amber,
  red, blue); `THEME_DEFAULTS: Record<token, hex>` (exact hexes in source; contrast
  rationale in comments — keep values byte-identical); hex regex accepts
  `#rgb|#rgba|#rrggbb|#rrggbbaa`.
- `Theme {name, colors: partial map}`; `ThemeState {theme: Theme|null, defaults}` —
  server serves BOTH; client merges (so "chosen" vs "inherited" stays distinguishable).
- `validateTheme(input)` — trims name (empty → 400); unknown tokens collected and
  named all at once with the full token list; bad hexes named with values; result
  rebuilt from validated keys only.
- `loadTheme(path)` — forgiving: missing/corrupt → null; valid tokens kept, invalid
  dropped individually. `saveTheme` (mkdir -p, pretty JSON + trailing newline),
  `clearTheme` (force rm), `themeState`.
- Handlers `getThemeH / putThemeH / deleteThemeH` (GET always 200; DELETE idempotent).

### `defaults.ts`
- `ModelDefaults {model: String|null, effort: Effort|null}`; `NO_DEFAULTS`.
- `loadDefaults(path = ~/.bough/model.json)` — forgiving read; blank model or
  unknown effort → null (independently). `saveDefaults` rebuilds cleanly (trim; no
  ride-along keys).

### `models.ts`
- `ModelCatalog {models: ModelRow[]}` (envelope). `TTL_MS = 10min`,
  `DEADLINE_MS = 2500`.
- `modelCatalog({now?, deadlineMs?, discover?})` — process-level cache + single
  in-flight discovery; a request races the discovery against a deadline timer; on
  timeout answers `mergeModels(MODELS, cached_stale_or_empty)`; the losing discovery
  still warms the cache for the next ask; a throwing discovery degrades to the
  static table; timer cleared on both paths (holds the runtime open otherwise).
- `resetModelCatalog()` (tests). Handler `getModelsH`.

### `fs.ts`
- `MAX_FILES = 20_000`; `MAX_ENTRIES = 2_000`.
- `listWorkspaceFiles(dir)` — two `git ls-files` passes, **tracked first**
  (`--cached`), then `--others --exclude-standard` deduped, capped. Any git failure
  → `[]`.
- `expandTilde`, `listDirEntries(dir)` — one level, dirs suffixed `/`, sorted,
  capped, unreadable → `[]`.
- Handlers: `listFilesH` (session workspace; no workspace → `{files: []}`),
  `listFilesForWorkspaceH`, `listDirEntriesH` (`base` resolves relative dirs
  against the session workspace), `branchH` (`git rev-parse --abbrev-ref HEAD`;
  detached HEAD or non-repo → `{branch: ""}`).

### `skills.ts`
- `SkillRow {name, description, source, dir, mcp?, error?}` (mcp omitted when empty;
  error present when SKILL.md malformed — listed, never omitted).
- `listSkillsH` → `{skills, sources}`; `getSkillH` → row + `body` with
  `${SKILL_DIR}` resolved; 404 message names installed skills and search dirs.

### `attachments.ts`
- `MAX_IMAGE_BYTES = 5MB`; accepted content-types: image/png→png, image/jpeg→jpg,
  image/gif→gif, image/webp→webp (nothing else, 400 naming the four).
- `uploadAttachment` — raw body bytes (NOT JSON); empty → 400; oversized → 400
  before disk; writes `~/.bough/attachments/<uuid>.<ext>` with `wx` (no overwrite);
  → 201 `{path, mediaType, name: "clipboard.<ext>", size}`.

### `workflows.ts`
Thin translations over `workflow/control.ts` / `run.ts` / `report.ts` / `saved.ts`
(those modules are the workflow subsystem's spec). Local Zod bodies:
`SaveWorkflowBody {name: min1}`, `PutSavedBody {script?, runId?}` (strict; neither →
400 with usage sentence), `RunSavedBody {sessionId, args?}`,
`SettingsBody {sizeGuideline}`. Handlers listed in §3.

### `main.ts` — boot sequence (order is load-bearing; see §4)
No exports. In Rust this is `main()`.

---

## 3. THE API CONTRACT — every route

Conventions: all bodies JSON unless noted; all errors are `{"error": "<message>"}`
with the status from the thrown `HttpError` (400 BadRequest/PathError, 404, 409
Conflict, 503 SearchIndexUnavailable, 500 unexpected). 405 carries `allow` header.
`Session`, `Message`, `Part`, `Turn`, `Usage`, `BackgroundJob`, `AskQuestion`,
`Schedule`, `WorkflowRun`, `WorkflowAgent` wire shapes are in `schema/parts.ts`
(camelCase field names; `.nullish()` fields may be absent or null — the Rust serde
types need `skip_serializing_if` + `Option`, matching TS's omit-when-absent).

### Events
| Route | Req | Resp |
|---|---|---|
| `GET /events[?sessionId=]` | — | SSE stream. Opens with `: connected\n\n`; each event is `event: <type>\ndata: <stamped envelope JSON>\n\n`; `: ping\n\n` every 15s. No `id:` field ever. Filter: session-scoped events of other sessions dropped; envelope-level `sessionId`-less events always pass. |

Envelope: `{type, sessionId?, seq, ts, data}` — `seq` process-monotonic (starts 1),
`ts` epoch ms, stamped by the bus at publish. Closed event-type set (16):
`session.created`, `session.updated` (data: Session), `session.activity`
(`{sessionId, activity: string|null}`), `message.started` (Message),
`message.delta` (`{messageId, delta}`), `message.part` (`{messageId, part}`),
`message.finished` (`{messageId}`), `message.retry` (`{messageId, attempt, reason}`),
`tool.log` (`{messageId, callId, line}`), `turn.finished` (`{turnId, sessionId,
status, error?}`), `ask.question` (AskQuestion), `job.spawned`/`job.exited`
(BackgroundJob), `workflow.updated` (WorkflowRun), `workflow.agent` (WorkflowAgent),
`workflow.log` (`{runId, line}`).

### Sessions & messages (`sessions.ts`)
| Route | Req | Resp |
|---|---|---|
| `GET /sessions` | — | `[SessionListItem]` — every non-collapsed session, newest first. |
| `GET /sessions?originId=<id>` | — | `[SessionListItem]` — EVERY branch of that origin (collapsed kinds AND forks), creation order. Unknown origin → 404 (not `[]`). |
| `POST /sessions` | `{title?, parentId?, kind?, workspace?, model?, effort?}` | 201 Session (the STORED row, re-read after pins). kind defaults `fork` if parentId else `root`; collapsed kinds refused 400 ("created by agent()/spawn(), not over HTTP"); unknown parent → 400; workspace `~`-expanded, must exist and be a dir (400 otherwise), sets `originDir` mirror + records git base (best-effort). Model/effort pins: body wins, else `~/.bough/model.json`, else unset. Publishes `session.created` with the stored row. |
| `GET /sessions/:id` | — | `{session, thread: [Message] (ancestors root→parent then own), usage: UsageTotals+{tree: UsageTotals}, effectiveModel, contextLimit: number\|null, primedTags: [..], projectRules: [..]}`. The reconnect path. `effectiveModel` = session.model ?? ctx.model ?? DEFAULT_MODEL. |
| `PATCH /sessions/:id` | `{model?: string\|null, effort?: "low..max"\|null}` | 200 Session. Absent = leave alone; explicit null = clear pin. Publishes `session.updated`. |
| `POST /sessions/:id/messages` | `{text, images?: [{path, mediaType, name, size}]}` | **202** `{message: Message, queued: bool}`. Empty text AND no images → 400. Consumes the draft (publishes `session.updated` for the clear). Parts: text part (if text) + one image part per image (paths, never bytes). Persists, indexes (search), publishes `message.started`. If session busy → `queued:true`, no turn started (drains later); else fires `ctx.startTurn` fire-and-forget — a throwing/rejecting starter is logged, response is still 202. |
| `PUT /sessions/:id/draft` | `{draft: string\|null}` | `{ok: true, draft}`. **No event published** (would race the writer's own composer). |
| `GET /sessions/:id/usage` | — | `{usage: UsageTotals, tree: UsageTotals}` — the poll-while-running cost meter; moves mid-turn per round. |
| `GET /model-settings` | — | `{defaultModel, cheapModel, defaultEffort: Effort\|null}` — pinned file first, then ctx.model, then built-in. |
| `PUT /model-settings` | `{model?: string\|null, effort?: null\|"low..max"}` | same as GET (re-answered after save). Partial: absent = keep, null = clear. |

`UsageTotals = {inputTokens, outputTokens, reasoningTokens, cacheReadTokens,
cacheWriteTokens, costUsd}`.

### Turn control (`turns.ts`)
| `POST /sessions/:id/interrupt` | — | 200 `{sessionId, interrupted: bool, message}`. Unknown session → 404. Idempotent; never waits for the unwind. |

### Questions (`questions.ts`)
| `GET /questions[?sessionId=]` | — | bare `[AskQuestion]`, oldest first, memory-only. |
| `POST /sessions/:id/questions/:qid` | `{answer: string}` or `{decline: true}` | `{ok:true, id, status: "answered"\|"declined"}`. Unknown/foreign/expired qid → 404 (message explains memory-only holds); empty/whitespace answer → 400; settled-meanwhile race → 409. |

### Jobs (`jobs.ts`)
| `GET /sessions/:id/jobs` | — | `{jobs: [BackgroundJob + tail fields]}` — session + transitive collapsed descendants; each row keeps its own `sessionId`; tail merged in non-destructively. |
| `POST /sessions/:id/jobs` | `{command: 1..4000 chars}` | 201 = parsed result of `registry.bashBg(name=command[..60], command, {sessionId, workspace}, {wake:false})` → `{id, name, pid}`. User shell: no turn, no wake, no thread entry. |
| `POST /sessions/:id/jobs/:jobId/kill` | — | `{message}` — SIGTERM + SIGKILL backstop, waits for death; `job.exited` follows via bus. Kill resolves across sessions (anything listable is killable). Unknown job → 404. |
| `GET /sessions/:id/jobs/:jobId/output` | — | whole retained buffer (head+tail with omission marker, as the model sees it); does NOT advance the model's cursor. Unknown → 404 (in-memory, dies with process). |

### Workflows (`workflows.ts`, `workflow/relaunch.ts`)
| `GET /workflows[?session=\|?sessionId=]` | — | `{workflows: [summary]}` (no script text), newest first. |
| `POST /workflows` | `{sessionId, script, args?}` | 201 WorkflowRun — the receipt, detached; progress on /events. |
| `GET /workflows/:id` | — | run detail: run + agents with live activity + mirrored script path + live-in-this-process flag (`workflowDetail`). Reconnect path for a run. |
| `POST /workflows/:id/stop` | — | 200 run row. Kills worker AND interrupts every subagent turn. Idempotent on finished runs. |
| `POST /workflows/:id/pause` | — | 200; **409 when not live in this process**. Gates NEW agent() calls. |
| `POST /workflows/:id/resume` | — | 200; parked calls release FIFO. |
| `POST /workflows/:id/rerun` | `{script?, args?}` | 201 `run + {replay: replaySummary}` — a NEW run; no script → edited `~/.bough/workflows/<id>.js` mirror wins. |
| `POST /workflows/:id/relaunch` | (see workflow spec) | new run seeded from a stopped run's journal, prefix-bounded replay. |
| `GET /workflows/:id/replay` | — | replay report (served/live counts). |
| `POST /workflows/:id/save` | `{name}` | 201 — saves the script the run would relaunch (mirror over row). |
| `GET /saved-workflows` | — | `{saved: [...]}` with `meta.description`. |
| `GET /saved-workflows/:name` | — | one saved workflow, script included. |
| `PUT /saved-workflows/:name` | `{script?}\|{runId?}` (strict) | 201; neither field → 400 usage message. Idempotent on name. |
| `POST /saved-workflows/:name/runs` | `{sessionId, args?}` | 201 `run + {savedAs}` — fresh run, no resumeOf, nothing replays. |
| `GET /workflow-settings` | — | `{sizeGuideline, target: number\|null, advice, tokenWarnThreshold, concurrency, maxAgentsPerRun, advisory: true}`. |
| `PUT /workflow-settings` | `{sizeGuideline}` | same minus concurrency/max. Advisory only — caps nothing. |
| `POST /workflows/:id/agents/:agentId/:action` | — | action ∈ {`stop`, `restart`} validated (typo → 400 explaining both); 200 outcome. |

### Schedules (`schedules.ts` — routes only; module is another subsystem)
| `GET /schedules` | — | bare `[Schedule]`. |
| `POST /schedules` | `{title, prompt, workspace?, spec, enabled?}` | 201 Schedule (spec grammar `every:<N><m\|h\|d>` / `daily@HH:MM` validated by the module). |
| `PATCH /schedules/:id` | all optional; `workspace: null` clears | 200 Schedule. |
| `DELETE /schedules/:id` | — | `{ok: true, removed: id}`. |

### Artifacts & comments
| `GET /sessions/:id/artifacts` | — | `{artifacts: [...]}` — filesystem-walk, newest first; deliberately no session-row check (artifacts outlive rows). |
| `GET /artifacts/:id/:path*` | — | the file. HTML (incl. sniffed extensionless) gets the comment widget injected; `cache-control: no-cache`; traversal → 403 text "forbidden"; missing → 404 (HTML page for browsers by `Accept`, JSON otherwise); a directory is a 404, never a listing. Percent-decoding per segment. |
| `GET /sessions/:id/comments[?artifact=]` | — | `{comments: [ArtifactComment]}`. |
| `POST /sessions/:id/comments` | `{artifact, text, anchor?}` | 201 ArtifactComment. Unknown session → 404 (no stray file). |
| `POST /sessions/:id/comments/send` | `{ids?}` | `{sent: n, wake}` — one `[artifact comments]` system note for the whole unsent (or named-subset) batch, THEN markSent. Empty batch → `{sent: 0}` (no error, no note). Unknown session → 404. |
| `DELETE /sessions/:id/comments/:cid` | — | `{ok: true}`; unknown → 404. |

### History ops (handlers live in `history/*` — that subsystem's spec; routes + status contract here)
| `POST /sessions/:id/fork` | `ForkBody {atMessageId, atPart?, editedText?, exclusive?, summarizeAbandoned?}` | **201 creates a session; source byte-identical.** Fork point must be the session's OWN message (ancestor → 400). |
| `POST /sessions/:id/unsend` | `{atMessageId}` | the ONE deleting route: session's own last USER message + everything after, within the TUI's `UNSEND_MS` gesture window. |
| `POST /sessions/:id/compact` | `{picks: [PartPick], instructions?}` | 201 compaction branch; source untouched. |
| `POST /sessions/:id/sections` | `{turns: [{gist ≤500}] 1..500}` | STATELESS: labeled ranges, index i = turn i; reads/writes nothing. Nested under session so typos 404 before an LLM call. |
| `POST /sessions/:id/extract` | `{picks}` | 201 fresh ROOT; picks may reach ancestors. |
| `POST /sessions/:id/move-into` | `{sourceId, picks}` | **200** (creates nothing) — appends copies onto `:id`; source keeps its turns. |
| `POST /sessions/:id/handoff` | `{goal}` | 201 fresh root with drafted opening prompt (stored as the new session's `draft`). |

`PartPick = {messageId, parts?: [nonneg int, min 1]}`.

### Changes rail (`changes.ts`)
| `GET /sessions/:id/changes` | — | always 200: `{available, reason?, base, files: [FileDiff], workspace}` (noise-filtered). Non-repo/no-workspace = answer, not error. |
| `POST /sessions/:id/changes/revert` | `{paths?: [String]}` | `{reverted, skipped, failed: [{path, error}]}`. Absent paths = revert ALL shown; `paths: []` → 400 refusal; out-of-set paths → skipped. Posts no-wake system note when reverted.len > 0. No change set → 400 with the rail's own reason. |

### Search (`search.ts`)
| `GET /search?q=&sessionId=&limit=` | — | `SearchResult` (shape §2). Empty q → 400 syntax hint; bad limit → 400 flattened issues; unknown sessionId → 404; broken index → 503; FTS syntax → auto-quoted retry reported via `rewritten`/`effectiveQuery`; degraded index reported via `index{…, degraded:true, repair}` until reindex. |
| `POST /search/reindex` | — | `{rebuilt: true, messages, sessions}` — corpus counts, not FTS rows. |

### Models & settings
| `GET /models` | — | `{models: [ModelRow]}` — static table merged with discovered rows (static ids win position); 2.5s deadline; never blocks boot. |

### Files / fs (`fs.ts`)
| `GET /sessions/:id/files` | — | `{files: [repo-relative]}` — git ls-files, tracked first; no workspace → `[]`; unknown session → 404. |
| `GET /files?workspace=<dir>` | — | same, session-less (the new-conversation screen). Missing param → 400. |
| `GET /fs/entries?dir=<path>[&base=]` | — | `{entries: ["name", "dir/"]}` — one level, sorted, dotfiles included, ≤2000; unreadable → `[]`. |
| `GET /fs/branch?dir=<path>` | — | `{branch: ""\|name}` — "" for detached/non-repo. Missing dir → 400. |

### MCP (routes; handlers in `mcp/*` spec)
`GET /mcp/servers` and every mutation answer the SAME `{registry, auth, active,
connections}` document. `PUT /mcp/servers`, `PUT/DELETE /mcp/servers/:name`
(`PutMcpServerBody` = `{command, args?, env?}` | `{url, headers?}`),
`POST .../connect`, `POST .../tools/:tool` (grant enforced in handler),
`POST .../restart`, `POST .../enable` / `.../disable` (`{sessionId, ttl?}`).
OAuth: `GET <CALLBACK_PATH>` (browser redirect target — must exist on bough's own
port), `GET/POST/DELETE /mcp/servers/:name/auth` (status / begin / forget). No route
ever returns a token.

### Theme (`theme.ts`)
| `GET /theme` | — | 200 always: `{theme: Theme\|null, defaults: {18 tokens}}`. |
| `PUT /theme` | `{name: 1..80 trimmed, colors: {token: hex}}` | 200 `{theme, defaults}`; unknown tokens/bad hex → 400 naming all offenders + the real token list; failed PUT does not overwrite. |
| `DELETE /theme` | — | 200 `{theme: null, defaults}`; idempotent. |

### Cheap tier & skills
| `POST /sessions/:id/ghost` | `{prefix?}` | ALWAYS 200 for an existing session: `{ghost: string\|null}` — null stands in for every failure (never an error banner). POST because the half-typed prefix must not enter URLs/logs. |
| `GET /skills` | — | `{skills: [SkillRow], sources: [{source, dir}]}` — fresh walk each call. |
| `GET /skills/:name` | — | SkillRow + `{body}` (`${SKILL_DIR}` resolved); 404 lists installed + dirs; traversal is just "unknown". |

### Attachments
| `POST /attachments` | raw image bytes, `content-type: image/{png,jpeg,gif,webp}` | 201 `{path, mediaType, name: "clipboard.<ext>", size}`; wrong type/empty/>5MB → 400. |

---

## 4. Behaviors & edge cases (mined from tests + code)

**Dispatch (`app.test.ts`, `http.test.ts`)**
- Matching is pathname-only; query string never affects routing. First match wins.
- Params: named groups extracted; an optional group that did not match is ABSENT
  from the map. `:path*` matches multi-segment.
- The one catch maps every `HttpError` status (incl. unusual ones like 503); async
  rejections too; non-Error throw values survive; one failing request doesn't
  poison the next. Unexpected errors are reported AND answered 500 with the message.
- 405 lists allowed methods (deduped) in body and `allow` header. `GET /` root
  pointer wins over a 405 when other methods own `/`.
- Route table has no duplicate (method, pathname) pair (pinned by test).
- `parseBody`: malformed JSON → fallback (default null) → schema decides the 400.
  An all-optional schema needs fallback `{}`.

**SSE (`events.test.ts`)**
- Never emits `id:`. Multi-line payload stays one `data:` line. Every declared event
  type frames. Unencodable payload: skipped + reported, connection lives.
- Heartbeat: comment frame per tick; cleared on disconnect; a tick after teardown is
  inert. N connect/disconnect cycles leave `bus.size == 0` (the leak check).
  Concurrent streams unsubscribe independently. A request aborted before body start
  subscribes to nothing. Teardown idempotent across abort+cancel. Abort also closes
  the stream controller so body readers see EOF.
- Bus: publish stamps `{seq: ++counter, ts: now()}` into a FRESH object (input not
  mutated); synchronous fan-out in subscription order; throwing listener isolated
  (and a throwing error-reporter also swallowed); listener unsubscribed during
  fan-out does not receive the in-flight event.

**Sessions (`sessions.test.ts`)**
- `subagent`/`workflow_agent`/`schedule_run` absent from GET /sessions, present
  under origin drill-in; roots/forks/compactions/`shell` always listed. No stored
  visibility flag exists.
- POST /sessions announces exactly the stored row (byte-for-byte re-read after
  pins). Pins are stored BEFORE the announce.
- Workspace: must exist, must be a directory, each with its own 400 message;
  `~` expands.
- postMessage: message lands + is announced + indexed even with NO starter wired;
  starter throw/reject contained; busy session → queued (starter not called);
  posted message keyword-searchable immediately; image-only ok, fully-empty 400;
  first post consumes handoff draft and announces THAT clear.
- Listing `busy`/`lastTurnStatus` derived from turn rows, never columns; `tokens`
  omitted when 0 and excludes cache traffic.
- Draft PUT: stores, emits nothing; null clears; 404/400 on bad session/shape.
- PATCH persists model AND effort to the row (not just echoing).

**Interrupt (`turns.test.ts`)** — aborts + reports true; idle session →
`interrupted:false` 200; double-tap safe; unknown session 404.

**Questions (`questions.test.ts`)** — fresh client rebuilds cards from GET;
answer settles the parked program; decline rejects catchably; guessing another
session's qid → 404; empty answer 400; second answer 409.

**Jobs (`jobs.test.ts`)** — transitive walk collects subagents' subagents and
schedule firings, NOT forks; lineage cycle does not hang; kill emits `job.exited`
and resolves a subagent's job via the spawner's URL; output/tail reads never steal
the model's `bashOutput` cursor; user-started shell (`!cmd`) never wakes the model.

**Artifacts (`artifacts.test.ts`)** — traversal in name OR session id blocked
(403 route-level); sessions cannot cross-read; listing survives DB reset and is
newest-first; comments sidecar never listed/served; extensionless HTML sniffed;
percent-encoded names round-trip; 404 page self-contained (no external refs).

**Comments (`comments.test.ts`)** — corrupt sidecar → empty; bad anchor → centered
default (text is the point); traversing session id cannot steer the write; send
posts ONE note and marks sent; subset send leaves the rest; widget self-contained
and interpolation-free.

**Search (`search.test.ts`)** — implicit AND; quoted phrase adjacency; density
ranking; scope 404 (not empty); limit clamp; parse-fail rewrite reported; embedded
quote escaping; FTS write failure doesn't break message insert; missing index is
503 about the INDEX (never a 400 about the query); degradation reported on every
search until rebuild heals; wrapper delegates all other methods; rebuild ==
incremental output; recovered-turn indexing skips missing messages; hit with a
missing message dropped.

**Theme / defaults** — see §2; every test name matches a §2 clause. Notably: failed
PUT does not overwrite; unknown effort dropped while the model beside it survives.

**Models (`models.test.ts`)** — discovered rows land AFTER the static table which
keeps its ids; hung provider answers static within deadline; timed-out discovery
still warms cache; single flight under concurrency; fresh cache not re-discovered,
expired one is; throwing discovery degrades.

**Naive-port traps**
- 202 (not 200) for postMessage; 201 for creates (sessions, forks, workflow
  start/rerun/save, comments, attachments, shells) — the TUI branches on these.
- `queued` must be computed from `busySessionIds` BEFORE deciding to start a turn.
- Bun's `Bun.serve` needed `idleTimeout: 0` — SSE is idle between turns and `bough
  exec` holds one request for the whole turn (default 900s). Axum/hyper: make sure
  no read/idle timeouts are configured on the listener.
- Omit-when-absent JSON fields (`costUsd`, `tokens`, `lastTurnStatus`, `originId`,
  `mcp`, `error`, `index`, …) — clients distinguish absent from null in places
  (PATCH semantics). Use `Option` + `skip_serializing_if` and, for PATCH bodies,
  a triple-state (absent / null / value) — e.g. `Option<Option<String>>` with
  `serde_with::rust::double_option`.
- `paths: []` vs absent `paths` on revert are OPPOSITE meanings.
- `URLPattern` semantics: `:name` = one segment, `:name*` = rest-of-path (may be
  empty and then absent from params). Percent-decode per segment (artifacts only —
  other params are used raw).
- The bus stamps seq/ts; PERSIST FIRST, THEN PUBLISH everywhere.
- Search-safe DB wrapper must be installed on the ctx handle only, AFTER boot
  recovery used the raw one.
- Interrupt returns before the turn unwinds; the row/event lag is by design.

---

## 5. Dependencies

Imports (what `server/` reaches DOWN to): `errors.ts`, `types.ts` (AppCtx/Db/Bus),
`schema/{parts,requests,events}.ts`, `paths.ts` (boughHome, attachmentsDir,
commentsDir/-PathFor, themePath, modelSettingsPath, workflowsDir, dbPath, confine),
`bus.ts`, `db/db.ts` + `db/extensions.ts` (main only), `hostfn/{jobs,ask,artifact}.ts`,
`turn/{runner,queue,state}.ts`, `agents/{notes,caps}.ts`, `hostfn/delegate.ts` +
per-verb hostfn factories (main only), `workflow/{control,run,report,saved,journal,
relaunch,schema}.ts`, `vcs/repodiff.ts`, `history/{compact,sections,fork,unsend,
extract,move,handoff,stats}.ts` (route table + sessions), `mcp/{client,config,oauth,
manager,service,status}.ts` (main + route table), `worker/{titles,ghost,activity}.ts`,
`skills/skills.ts`, `llm/{client,pricing}.ts`, `prompt/{assemble,project}.ts`,
`schedules.ts`, `scratch.ts`, `history/embed.ts`.

Imported by: only entry points import `app.ts` (pinned by a guard test); `turn/`,
`hostfn/`, `history/` NEVER import `server/` — they throw `HttpError`s instead.
Preserve that direction in Rust: an `HttpError`-equivalent enum lives in a shared
crate; only the server crate converts it to a response.

---

## 6. External deps → Rust equivalents

| TS/Bun | Used for | Rust |
|---|---|---|
| `Bun.serve` (idleTimeout 0, fetch handler) | listener | `axum` + `tokio` (`TcpListener::bind("127.0.0.1:port")`); no timeouts middleware |
| `URLPattern` | route matching | axum router paths (`/{id}`, `/{*path}`); or keep an ordered `matchit`/hand-rolled table if append-only ordering semantics are wanted — axum's router is fine because the table has no order-dependent overlaps except `/saved-workflows` vs `/workflows/:id`, which axum disambiguates statically |
| `ReadableStream` SSE | /events | `axum::response::sse::{Sse, Event, KeepAlive}` — but note: axum's `Event::id` must never be set; keep-alive comment text `: ping`; initial `: connected` comment via a first stream item; teardown = stream `Drop` (tokio mpsc receiver dropped → unsubscribe) |
| Zod schemas | body validation | `serde` + hand-rolled checks (or `validator`); reproduce the "invalid body: …" 400 text shape loosely — clients only read `{error}` |
| `zod` open records | theme colors, MCP env | `HashMap<String, String>` |
| `node:fs` sync IO | theme/defaults/comments/attachments | `std::fs` (these are small files; sync in a `spawn_blocking` or just tolerate short blocking) |
| `Bun.spawn` git | fs.ts listings | `tokio::process::Command` (`git ls-files`, `rev-parse`), failure → empty vec |
| `crypto.randomUUID()` | ids | `uuid::Uuid::new_v4()` |
| `Proxy` wrapper (searchSafeDb) | swallow indexMessage | newtype `SearchSafeDb<D: Db>` delegating; interior `Mutex<IndexHealth>` |
| `TextEncoder` | SSE bytes | `String`/`Bytes` |
| in-process `Bus` | fan-out | `tokio::sync::broadcast` (each SSE task filters) or a `Mutex<Vec<mpsc::Sender>>` for closer semantics (per-subscriber isolation, unsubscribe-on-drop, `size()` for the leak test). Broadcast is simpler; note broadcast lag drops events — acceptable (display transport), but the TS bus never drops; prefer unbounded mpsc per subscriber |
| `setInterval` heartbeat | SSE | `tokio::time::interval` inside the stream (or axum `KeepAlive::new().interval(15s).text("ping")`) |
| `setTimeout` race | models deadline | `tokio::time::timeout` around a shared `OnceCell`/`Mutex<Option<JoinHandle>>` in-flight discovery |
| module-level mut state (models cache, jobs registry seam) | | `OnceLock<Mutex<…>>` or fields on the server state — prefer putting them on AppCtx in Rust; the TS "AppCtx frozen" excuse doesn't apply |

---

## 7. Suggested Rust layout (crate `bough-server`)

```
server/
  mod.rs        — AppCtx struct (db: Arc<dyn Db>, bus: Arc<Bus>, model/effort,
                  now: fn, cheap: Option<Arc<dyn CheapTier>>, start_turn: seam,
                  turn_registry, job_registry, model_defaults_path, workflow_control…)
                  ← fold the TS `WithXxx` ctx-extension traits into optional fields
  error.rs      — HttpError enum {BadRequest, NotFound, Conflict, Path,
                  SearchIndexUnavailable, …} + IntoResponse ({"error": msg})
  http.rs       — json(), parse_body<T> (against serde), 405/404 fallback layer
  app.rs        — Router construction (the §3 table), the one catch = a
                  middleware/`Result<_, HttpError>` at each handler
  events.rs     — SSE handler + passes_filter + frame; heartbeat
  sessions.rs, turns.rs, questions.rs, jobs.rs, artifacts.rs, comments.rs,
  changes.rs, search.rs, fs.rs, models.rs, skills.rs, theme.rs, defaults.rs,
  attachments.rs, workflows.rs
  main-boot in bin: open db → bus → recover orphaned turns/workflows → wire
  start_turn (ONE composed builder, not TS's seven supersessions — see below) →
  sync mirrors → schedule ticker → searchSafeDb swap → cheap tier watchers →
  bind 127.0.0.1 → boot report logs
```

- `Db` and `Bus` are traits (ports) in a shared `bough-core` crate; wire shapes
  (`Session`, `Message`, `Part` enum with `#[serde(tag = "type")]`, events enum) in
  `bough-schema`, shared with the ratatui client.
- Handler shape: ordinary axum handlers taking `State<Arc<AppCtx>>` +
  `Path`/`Query`/`Json`, returning `Result<impl IntoResponse, HttpError>`.
- **Boot composition**: `main.ts` rebuilds the turn starter seven times because its
  boot section is append-only for parallel TS agents. Port ONLY the final
  composition: skill-aware per-message starter with `granted = BASE + workflow +
  schedule + ask + state + artifact`, workflow verb gated on `delegationTier ==
  top`, `deliver = note deliverer`, `assemble` resolving skills + MCP catalog per
  turn (session id captured at start), `survivingJobs` from the job registry, and
  `skillsFor` wrapped so a resolution failure logs and yields empty (a throw there
  would strand the session busy forever).
- Boot ORDER that matters: sqlite extensions before first open; recover orphaned
  turns AND workflows before bind; note orphaned subagents (recorded, not woken);
  searchSafeDb installed on ctx.db before serving (raw handle for recovery);
  ticker started after the starter exists; shutdown kills background shells and
  MCP children (SIGINT/SIGTERM handler + on-exit).
- Async boundaries: everything request-scoped is async; bus publish stays
  synchronous-ish (lock, iterate, send non-blocking); turn starting is
  `tokio::spawn` fire-and-forget with the error logged.

---

## 8. v1 scope cut

Core (needed for a working agent loop + TUI):
`app/http/error` dispatch, `GET /events`, sessions CRUD + `postMessage` +
`getSession` + draft + usage, interrupt, jobs (list/kill/output — registry comes
with the hostfn subsystem), questions, theme GET (TUI boots by fetching it — can
serve `{theme:null, defaults}` statically), `GET /models` (static table only, no
discovery), model-settings GET/PUT, `defaults.rs`, fs listings (the composer's `@`),
attachments.

Stub in v1 (route exists, honest minimal answer):
- Workflows: all routes → 404/`{workflows: []}` until the workflow subsystem lands.
- MCP + OAuth: `GET /mcp/servers` → empty document; mutations 501-ish 400.
- Search: `GET /search` → `SearchIndexUnavailableError`-style 503 or empty result;
  skip searchSafeDb wrapper until FTS lands (then it's mandatory).
- Skills: `{skills: [], sources: []}`.
- Ghost: `{ghost: null}` (contractually always valid).
- History ops (fork/compact/sections/extract/move/handoff/unsend): 400 "not yet".
- Changes rail: `{available:false, reason:"…", …}` is a legal permanent answer.
- Artifacts/comments: can ship later; TUI degrades (no links to serve).
- Schedules: `[]` + 400 on create.

Drop from v1 entirely: model discovery (static MODELS table is the designed
fallback), comment widget injection (serve artifacts raw until comments port),
`primedTags`/`projectRules` on getSession (`[]` is the documented no-workspace
answer — but keep the FIELDS present), cheap tier (all readers degrade on absent),
embeddings drain ticker, MCP grant promotion, scratch sweep, script mirrors.

The contract to never cut: status codes (202/201/200/4xx), the `{error}` envelope,
the SSE framing rules (no `id:`, global events pass filters), derived visibility on
listings, and persist-then-publish ordering.
