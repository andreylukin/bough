# Port spec: `cli` — headless subcommands (exec, patterns, mcp, sync-mcp, tags)

Source: `src/cli/{exec,patterns,mcp,sync_mcp,tags}.ts` (+ their `.test.ts`).
Dispatcher: `scripts/bough` (bash) — routes `bough exec|mcp|sync-mcp|tags|patterns` to
`bun src/cli/<module>.ts "$@"` after `require_bun`; in the Rust port these become
subcommands of the single `bough` binary. The bash script's `ensure_up` auto-starts the
server for `exec` and `mcp` **unless `--port` appears in argv** (an explicit port means
the caller manages that server, e.g. the bench). `sync-mcp`, `tags`, `patterns` never
need the server.

Shared conventions (stated in every file header; they ARE the architecture):

1. **Argument parsing is pure and total.** Each `parse*Args` is a plain function
   `&[String] -> Result<Args, UsageError>` (some also return a Help variant). Never reads
   the environment, never exits, never throws/panics — every malformed input becomes a
   usage-error string.
2. **Every effect is injected.** Each `run*` takes a deps struct (fetch, stdout writer,
   stderr writer, stdin reader, env lookup, cwd, clock, sleep…) and **returns an exit
   code**; only the `main` entry touches a real process. This is what lets every command
   be tested against the real route table over an in-memory DB with no socket bound.
3. Stdout carries the answer; stderr carries diagnostics/warnings/usage. `--json` output
   is stable and parseable.

---

## 1. Purpose & invariants (quoted)

### exec.ts — `bough exec [flags] "prompt"`
> "THE INVARIANT THIS FILE HOLDS … **the event stream is opened BEFORE the prompt is
> posted.** The server answers `POST /sessions/:id/messages` with a 202 and runs the turn
> behind it, reporting over `/events` — and `/events` has no replay by design (`seq` is a
> dedupe key, not a resume cursor). A subscriber that attaches after the turn has already
> published `turn.finished` will never see it … the CLI waits out its full `--timeout`
> and exits 1 on a turn that actually succeeded — and it only does that for turns fast
> enough to finish inside the post — which is to say, for the cheapest and most-tested
> prompts, intermittently."

> "Second invariant: **every effect is injected** … `runExec` … RETURNS an exit code
> rather than calling `process.exit`."

> "Third: **argument parsing is pure and total.**"

> "Fourth: **a timeout STOPS the turn it abandoned.** … the timeout path raises
> `POST /sessions/:id/interrupt` on a short deadline of its own and reports what actually
> happened … The exit code does not move: a turn this client gave up on did not complete,
> whether or not the stop landed."

### patterns.ts — `bough patterns [FILE]`
> "compress a log into the handful of statements it is actually made of." Named
> `patterns` not `logs` because `bough logs` already tails the server's own log. A
> subcommand, not a host function, on purpose ("a host function is a permanent widening
> of every program's API … a subcommand costs nothing until something runs it").
> "There is deliberately no 'found errors' exit code."
> Lines stream through the analyzer and are "never collected" — buffering "silently caps
> the tool at whatever fits in memory — a 48MB log costs ~700MB as an array of strings."

### mcp.ts — `bough mcp <verb>`
> "THE VERB THAT MATTERS IS `doctor` … the real question is never about one server — it
> is 'why is none of this working'." Causes are few and each has a different fix: not
> granted, no credential, a stale credential another client owns, a credential that was
> never there, an endpoint that refuses.
> "The 0/1 split is what makes this usable in CI: `bough mcp doctor` exits non-zero when
> the setup needs a human, and zero when it does not."

### sync_mcp.ts — `bough sync-mcp`
> "THE INVARIANT THIS HOLDS … non-negotiable: **what gets written down is a REFERENCE,
> never a secret.** bough's registry is served by `GET /mcp/servers` … so a token copied
> into it would sit in a response body and, from there, in the model's context."
> "EVERY SERVER GETS THE CREDENTIAL THAT IS ACTUALLY ITS OWN. … The generalization this
> refuses — 'it is remote, so give it the bearer token' — would post the user's Anthropic
> credential to whatever third party a config file names."
> "WHAT IT NEVER DOES: overwrite. A name already in bough's registry is left exactly as
> it is and reported … `--force` is how you say otherwise. Nothing here grants anything
> either."
> "Running a sync twice must be the same as running it once." (idempotency by endpoint
> identity, including stdio commands)

### tags.ts — `bough tags`
> "The default view IS the priming note's ranking, with the numbers it sorted by."
> "NO SERVER, AND ONLY READS. The database is opened directly — every query here is a
> SELECT, and they live in `db/db.ts` beside the ones the prompt uses so the ranking this
> prints cannot drift from the ranking the model gets."
> `sql` verb: "READ-ONLY IS ENFORCED TWICE, both at the connection: the handle is opened
> `{readonly: true}` AND `PRAGMA query_only = ON`, which also covers anything a clever
> statement ATTACHes. The keyword check on top exists only to answer a write attempt with
> a sentence instead of a bare SQLITE_READONLY."

---

## 2. Public API

### exec.ts
- `DEFAULT_TIMEOUT_SECONDS = 900`, `DEFAULT_PORT = 4321`, `USAGE: string` (must name
  every flag and contain "no sandbox" — pinned by test).
- `interface ExecArgs { prompt: string; workspace?: string; model?: string; json: bool; timeoutMs: number; port?: number }`
  — `prompt` verbatim; empty or `-` defers to stdin; `timeoutMs` already validated.
- `interface ExecUsageError { usageError: string }`; `interface ExecHelpRequest { help: true }`
  (help is stdout + exit 0, distinct from usage error which is stderr + exit 2).
- `parseExecArgs(argv) -> ExecArgs | ExecUsageError | ExecHelpRequest` + guards
  `isHelpRequest`, `isUsageError`.
- `createSseReader() -> impl FnMut(&str) -> Vec<SseFrame>`; `interface SseFrame { name: string; data: json }`
  — incremental, buffer-until-`\n\n`, CRLF normalized, per-frame parse.
- `interface ExecDeps { fetchFn, write(stdout), warn(stderr), readStdin, stdinIsTerminal, env(name), cwd, realPath }`.
- `interface ExecEnvelope { session, status: TurnStatus|"timeout", ok: bool, text, error?, usage?: UsageTotals, treeUsage?: UsageTotals }`.
- `runExec(argv, deps) -> exit code (0|1|2)`.
- `realDeps()` — the only impure constructor.
- Internal: `stopTurn(api, sessionId, deps) -> bool` (own 5s `INTERRUPT_TIMEOUT_MS`
  AbortController — the run's deadline is already aborted by definition), `payloadOf`
  (SSE frame carries a stamped `BoughEvent`; the payload is at `.data`), `cancel`
  (best-effort reader release).

### patterns.ts
- `type Format = "llm" | "json" | "human"`.
- `interface PatternArgs { file?: string; format?: Format; top: number (default 20); colour?: bool; threshold?: number; refYear?: number }`.
- `parseArgs(argv) -> PatternArgs | UsageError` — `--help` returns `{ usageError: "" }`
  (empty string = help sentinel: printed to stdout, exit 0).
- `interface PatternDeps { readLines(file?) -> (async) line iterator; out; err; isTty: bool; width?: number }`.
- `runPatterns(argv, deps) -> 0|1|2`.
- Uses `Analyzer` from `src/logs/analyze.ts` (`push(line)`, `finish() -> Analysis`) and
  `toHuman/toJson/toLlm` from `src/logs/format.ts` — the logs subsystem owns those.

### mcp.ts
- `VERBS = ["list","test","auth","logout","grant","revoke","add","remove","doctor","call"]`,
  `type McpVerb`.
- `interface McpArgs { verb; name?; url? /* add: URL; call: TOOL name */; argsJson? /* call: 4th positional */; json: bool; port?; timeout: number (default 180, auth only); session? }`.
- `parseMcpArgs(argv) -> McpArgs | McpUsageError` — no verb at all = `list`.
- `interface McpDeps { fetch, out, err, env: map, sleep?, stdin?, now? }`.
- `runMcp(argv, deps) -> 0|1|2`.
- Internal: `glyph()` (`●` connected / `◐` granted-not-connected / `○` not granted —
  deliberately the panel's vocabulary), `diagnose()` (the doctor brain, see §4),
  `sessionOf()` (`--session` wins, else `$BOUGH_SESSION`), `base()` (port from `--port`
  else `BOUGH_PORT` else 4321), `call()` (one fetch; a network error prints
  `no bough server at <host> … Start one: bough start` and returns None ⇒ exit 2),
  `errorOf()` (route's `body.error` string, else `HTTP <status>`).

### sync_mcp.ts
- `interface KeychainGrant { key, name, url, expiresAt?, empty: bool }` — `empty` is a
  boolean, never the token ("a secret that is never loaded cannot be logged").
- `isStale(grant, now) -> bool` (`expiresAt <= now`).
- `holdsGrants(value: string) -> bool` — store-selection predicate: does this credential
  item's JSON hold a non-empty `mcpOAuth` map? Passed to `credentialReaderFor` so the
  read targets *the store with the grants*, not "the item" (keychain may hold
  `claudeAiOauth` while grants moved to `.credentials.json`).
- `readGrants(read: KeychainReader) -> { grants, note: string|null }` — code 128 = access
  denied (macOS dialog dismissed) ⇒ warn "re-run and choose Allow"; code 44 = no such
  item ⇒ silent; unparseable JSON ⇒ note. A read failure is NOT an error — config-file
  sync still proceeds.
- `interface Found { name, server: ClaudeServer, source: string }`.
- `interface SyncResult { name, source, action: "added"|"updated"|"skipped"|"failed", authed?, renamedFrom?, reason? }`.
- `interface SyncArgs { dirs: Vec<String>, force, dryRun, help, plugins (default true) }`.
- `parseSyncArgs(argv) -> {args} | {usage}` — flags only; any bare word is a usage error.
- `collectClaudeServers(dirs, readJson, home, configDir) -> { found, errors }`.
- `collectPluginServers(configDir, dirs, readJson) -> { found, errors }`.
- `toBoughServer(claudeServer, grant?) -> { server, authed } | { reason }`.
- `boughName(raw, taken: &Set) -> Option<String>` — slug derivation (see §4).
- `looksSecret(key, value) -> bool` — `/(token|secret|key|password|passwd|credential)/i`
  on the key AND `value.len() >= 12`, but false when value is already `${…}`.
- `interface SyncDeps { readJson?, keychain?, config?: McpConfigOptions, home?, configDir?, cwd?, out?, err? }`.
- `runSyncMcp(argv, deps) -> 0|1|2`.

### tags.ts
- `type TagsVerb = "list" | "show" | "stats" | "sql" | "similar"`.
- `MAX_ROWS = 200` (bounds `sql` and `similar` output).
- `SURFACE` string: `"command_history, command_tags, command_dirs, command_history_fts, messages, messages_fts, sessions, turns"` — quoted in every sql refusal/error.
- `interface TagsArgs { verb; tag? /* also holds sql query / similar text */; repo?; allRepos; limit (20); days (30); json; program }`.
- `parseTagsArgs(argv) -> TagsArgs | UsageError` — `-h/--help` returns
  `{usageError: USAGE}` and `runTags` maps *exactly-USAGE* to exit 0 (help), anything
  else to exit 2.
- `interface TagsDeps { db?: Db, dbFile?, embed?: factory -> Option<layer{similar(text) async, close()}>, cwd?, now?, out, err }`.
- `runTags(argv, deps) -> 0|1|2` (async only because `similar` awaits the embed layer).

---

## 3. Data structures, DB tables, wire shapes (exact field names)

### HTTP routes exec drives (loopback `http://127.0.0.1:<port>`)
- `POST /sessions` body `{ title: "exec: <prompt[..48]>", workspace, model? }` → 200
  `Session` JSON (fields used: `id`; created session has `kind:"root"`,
  `workspace`/`originDir` = realpath'd dir, `model` if given). Non-2xx ⇒ exit 2
  (`bough refused the session: <status> <body>`).
- `GET /events?sessionId=<id>` → SSE stream. Each frame: `event: <type>` +
  `data: <json BoughEvent>`; the payload is `envelope.data`. Comment frames
  (`: connected`, `: ping`) carry no data.
- `POST /sessions/:id/messages` body `{ text: prompt }` → 202.
- `POST /sessions/:id/questions/:qid` body `{ decline: true }` — fire-and-forget.
- `POST /sessions/:id/interrupt` → `{ interrupted?: bool }` — only `interrupted === true`
  counts as "stopped".
- `GET /sessions/:id` → `{ usage?: UsageTotals & { tree?: UsageTotals } }` — the
  authoritative post-turn record ("cache splits that decide the cost are only summed
  once the turn ends"); `tree` is split off into `treeUsage`.

Events consumed: `message.delta { messageId, delta }`, `message.retry { messageId, attempt, reason }`,
`ask.question { id, sessionId, messageId, question, status, ts }`,
`turn.finished { turnId, sessionId, status, error? }`.
`TurnStatus = "running"|"done"|"error"|"interrupted"|"orphaned"`.
`UsageTotals = { inputTokens, outputTokens, reasoningTokens, cacheReadTokens, cacheWriteTokens, costUsd }`.

`ExecEnvelope` (one line on stdout under `--json`):
`{"session","status","ok","text","error"?,"usage"?,"treeUsage"?}` — `status` may be the
literal `"timeout"`; envelope is printed even when the usage fetch fails (absence of
`usage` is the report).

### Routes mcp drives
- `GET /mcp/servers` → `McpStatus = { registry: { servers: Record<name, ServerConfig> }, auth: Record<name, { authorized: bool }>, active: string[], connections: ConnStatus[] }`.
  `auth` is populated for remote (`url`) entries ONLY. `connections[i]` has at least
  `{ server, alive, toolCount, error? }`.
- `POST /mcp/servers/:name/connect[?session=ID]` → `ConnectResult = { server, connected, error?, tools?: [{name}] }`.
  A ≥400 body without a `connected` field is normalized to
  `{ server, connected: false, error: errorOf(r) }`.
- `POST /mcp/servers/:name/auth` → `{ status: "authorized" | "pending", authorizationUrl?, correctedUrl? }`;
  `GET …/auth` → `{ authorized: bool }` (polled every 1 s via injected sleep, wall clock
  via injected `now`, deadline `now + timeout*1000`).
- `DELETE /mcp/servers/:name/auth` — logout.
- `POST /mcp/servers/:name/enable` / `.../disable` body `{ "sessionId": "" }` — the empty
  string IS the global scope, deliberately ("a CLI does not [have a session on screen],
  and inventing one would make the verb mean something different from what it says").
- `PUT /mcp/servers/:name` body `{ url }` — add. `DELETE /mcp/servers/:name` — remove.
- `POST /mcp/servers/:name/tools/:tool[?session=ID]` body = parsed JSON args →
  `{ result }`; the CLI prints **`r.body.result ?? null`**, not the envelope
  (`--json` only switches to pretty-print, indent 2 vs 0).

### sync-mcp file shapes
Sources read (Claude Code's own scope order; later wins by name):
1. `~/.claude.json` → `mcpServers` (legacy user scope)
2. `$CLAUDE_CONFIG_DIR/.claude.json` (default `~/.claude/.claude.json`) → `mcpServers` — read last, wins
3. either file → `projects["<dir>"].mcpServers` for each `--from` dir
4. `<dir>/.mcp.json` → `mcpServers` (checked in; the last word)
5. installed plugins: `<configDir>/plugins/installed_plugins.json` →
   `{ plugins: { "<plugin>@<marketplace>": [ { installPath, scope?, projectPath? } ] } }`
   (value may be a single object or array; `scope:"project"` taken only when
   `projectPath ∈ dirs`); per install:
   `<installPath>/.claude-plugin/plugin.json` → `{ name?, mcpServers? }` and
   `<installPath>/.mcp.json` in EITHER shape — `{ mcpServers: {...} }` wrapper OR a bare
   `{ "<name>": {...} }` map (terraform/linear/github ship bare). File beats manifest.
   `${CLAUDE_PLUGIN_ROOT}` is substituted with `installPath` in command/args/cwd/url/env
   values. Plugin server names are `plugin:<pluginName>:<serverName>` (verbatim — that is
   the keychain grant key), first install wins.
6. credential store `mcpOAuth` map, keyed `<serverName>|<hash>`, entries
   `{ serverName, serverUrl, expiresAt?, accessToken?, … }` (parsed permissively;
   tokens deliberately never read into memory beyond an emptiness check).

`ClaudeServer` schema (all optional, passthrough — "another tool's file"):
`{ type?, command?, args?: string[], env?: Record, cwd?, url?, headers?: Record, oauth?: { clientId? } }`.

Written registry entry (via `upsertServer(name, server, config)` into `~/.bough/mcp.json`,
`{ servers: {...}, activations: {...} }`):
- remote: `{ url, headers, clientId? }` where headers may gain
  `Authorization: "Bearer ${keychain:Claude Code-credentials#mcpOAuth.<key>.accessToken}"`
  (its own grant) or `"Bearer ${keychain:Claude Code-credentials#claudeAiOauth.accessToken}"`
  (account token, Anthropic hosts only). `CLAUDE_CODE_ITEM = "Claude Code-credentials"`.
- stdio: `{ command, args: [], env: {}, cwd? }` (defaults filled; registry also persists
  `headers: {}` on stdio entries — see test "a stdio server keeps its command…").
- remote-vs-stdio decision: `remote = url && (!command || type=="http" || type=="sse")`;
  neither ⇒ failed with reason "has neither a `command` nor a `url` bough can use".

### tags DB surface
Opened directly (no server): `paths.dbPath()` (`~/.bough/bough.db`) via `openDb()`.
Db methods used: `commandsForTag(tag, {repo?, limit}) -> TaggedCommand[]`,
`tagDiversityByDay(sinceTs, repo?) -> TagDiversityDay[]`, `programForMessage(messageId) -> string|null`.
Ranking via `history/stats.ts`: `workspaceRepo(cwd)` (repo identity: git origin URL or
path) and `rankedRepoTags(db, repo, now, limit) -> RankedTag[]`
(`{ tag, weight /* success × recency */, repos, score /* weight × idf */ }`).
`TaggedCommand = { ts, repo, cmd, tags, exitCode: number|null, durationMs, sessionId, messageId: string|null }`.
`TagDiversityDay = { day: "YYYY-MM-DD" local, sessions, commands, tagged, distinctTags, distinctRefs, tagUses, singletons }`.
`sql` verb opens a SECOND connection: `Database(path, readonly)` + `PRAGMA query_only=ON`
+ `PRAGMA busy_timeout=2000`, keyword pre-check strips leading `--` line comments,
`/* */` block comments and whitespace, uppercases first 8 chars, requires prefix
`SELECT` or `WITH`. `similar` uses `createEmbedLayer()` (sqlite-vec + lembed over
`embeddings.db`) — returns None when extensions absent.

---

## 4. Behaviors & edge cases (mined from tests + code — a naive port gets these wrong)

### exec
- **Call order is pinned**: `POST /sessions` → `GET /events` → `POST …/messages`. The
  test's fake server publishes `turn.finished` synchronously inside the post handler;
  reversed ordering observes nothing and times out. In Rust: the SSE response must be
  awaited (headers received, bus subscription live server-side) before posting.
- Exit codes: 0 = `turn.finished` status `done`. 1 = `error`/`interrupted`/`orphaned`,
  timeout, or stream closed early. 2 = usage error, unreadable stdin, bad
  `BOUGH_PORT`, connection failure at any of the three setup calls, server refusing
  session/stream/message, invalid `--workspace` (realpath throws).
- Prompt resolution: positional trimmed; `"-"` or (empty AND stdin not a TTY) reads all
  of stdin, trimmed. Empty final prompt (incl. empty invocation on a terminal — must NOT
  block on the keyboard) ⇒ usage to stderr, exit 2, and **no request made** (test
  asserts `calls == []`).
- Parse rules: `--flag=v` and `--flag v` both accepted; short `-w`/`-m` (also `-w=v`);
  a value flag consumes the next token **even if it starts with `-`** (model ids may);
  `--` ends flag parsing (prompt may start with a dash); bare `-` is the stdin sentinel
  not a flag; `>1` positional ⇒ error "expected one prompt … quote it as a single
  string" (a forgotten pair of quotes must not run a one-word prompt); unknown flags are
  errors (`--jsno` must stop, not stream); `--json` with a value is an error;
  `--timeout` must be finite and > 0 (fractional allowed, `Math.round(s*1000)`);
  `--port` integer 1..=65535. `-h`/`--help`/`--help` anywhere ⇒ help on stdout, exit 0 —
  but a prompt merely containing "help" is a prompt.
- Port: `--port` beats `BOUGH_PORT` beats 4321; a non-port `BOUGH_PORT` value ⇒ exit 2
  with `BOUGH_PORT is not a port number: <val>`.
- One `AbortController` deadline over the WHOLE run (session create + stream open +
  post + read + usage fetch); a `timedOut` flag distinguishes "deadline fired" (exit 1
  path with `timed out …` wording) from "socket died" (exit 2 at setup; at read time it
  is "the event stream closed before the turn finished" and still exit 1).
- `message.delta`: append to `text`; stream to stdout verbatim unless `--json`; track
  `streamed` so a final `"\n"` lands only if something streamed (deltas end mid-line).
- `message.retry`: reset `text = ""` (the message re-streams from the top; the envelope
  must carry the answer, not the false start), warn `[retry <attempt>: <reason>]` —
  stdout cannot be un-written, so the boundary is stderr-only.
- `ask.question` with `status == "pending"`: warn
  `[declined a question — bough exec is not interactive: <first line, ≤120 chars>]` and
  fire-and-forget POST `{decline:true}` (errors swallowed; blocking would be worse than
  the old wait-out-the-deadline behavior). Non-pending question events ignored.
- `turn.finished`: take `status` (default `"done"` if payload missing) and `error`,
  break the read loop, cancel the reader.
- Timeout/early-close: call `stopTurn` on its own fresh 5s AbortController ("the run's
  deadline has already fired by definition and its signal is aborted — reusing it would
  abort this request before it was sent"). Report `interrupted the turn in session <id>`
  vs `could NOT interrupt …` — `false` covers request-failed / nothing-running /
  stop-deadline-elapsed and is deliberately not distinguished. Exit stays 1 either way.
- Errored turn: `turn <status>: <error>` on stderr; partial text still reaches stdout
  (it is what the model actually said).
- `--json`: exactly one line; usage fetch is best-effort and must not change the exit
  code; on failure cancel the response body.
- SSE reader: chunk boundaries can fall mid-line/mid-frame — nothing is interpreted
  until its `\n\n` terminator arrives (buffer + `\r\n`→`\n` first). Per-block: comment
  lines skipped, `event:` sets name (default `"message"`), multiple `data:` lines join
  with `\n` (each `data:` prefix stripped then `trimStart`), no data ⇒ no frame,
  non-JSON data ⇒ frame dropped silently (one malformed payload must not end a turn).
  Do NOT track `event:` across frames (the old per-line parser mislabeled payloads).

### patterns
- Exit: 0 analyzed (including empty input and inputs full of ERRORs), 1 unreadable
  input, 2 usage. Help is exit 0 on stdout (internally the empty-string usageError).
- Flags before or after FILE both work; `-` = stdin explicitly (leaves file unset);
  second file ⇒ error; unknown `-x` ⇒ error; two *different* formats ⇒ error
  "cannot both be given" but a repeated identical format is fine; `--top` positive int;
  `--threshold` in (0, 1] — exactly 1.0 valid, 0 invalid; `--year` int 1970..=9999;
  `--no-color`/`--no-colour` both spelled; missing option value must become a usage
  error, never a crash on `Number(undefined)`.
- Default format: `human` when stdout is a TTY, else `llm` ("off a terminal … that
  something is far more often a model than a person running `less`"). Default colour =
  isTty; `--no-color` beats it.
- Streaming: lines feed `Analyzer.push` one at a time; the file reader must decode
  UTF-8 incrementally (multi-byte chars split across chunk boundaries must survive —
  Bun uses `TextDecoder{stream:true}`; Rust: `BufRead::lines` over a `BufReader` handles
  this) and yield a final unterminated line.
- Empty analysis (`lines == 0`): stderr `no log lines found`, exit 0, and under `--json`
  STILL print the JSON object (a consumer must not special-case the empty file).
- Rendered output has its single trailing newline stripped before `out()` (deps.out adds
  the line ending).
- Pinned output properties (owned by logs/, asserted here): `--top` truncates
  `patterns[]` but not `patternCount`; per-pattern `count`s sum to `lines`; llm view
  puts `## Problems` before `## Everything else`; no format emits ads/URLs/footers;
  no-timestamp logs have `timeSpan: undefined`; blank lines are skipped not clustered;
  llm view starts `# <n> lines`; human view contains `lines → N patterns`.

### mcp
- Exit: 0 fine (list, empty registry, healthy doctor, connected test, completed auth,
  grant/revoke/add/remove/logout success, call success); 1 operation ran and the answer
  is bad (failed connect, route ≥400 on action verbs, auth gave up/failed, doctor found
  a `bad` row); 2 usage OR no server on the port (network error anywhere prints
  `no bough server at <host> (<err>). Start one: bough start`).
- Parsing: bare `mcp` = `list`. `--port`/`--timeout` want a positive finite number;
  `--session` wants a value; unknown flags/verbs are errors; NEEDS_NAME verbs (all but
  list/doctor) without a name ⇒ "needs a server name"; `add` without URL and `call`
  without tool have their own messages. Positionals: `[verb, name, url, argsJson]` —
  for `call`, `url` slot is the tool name and `positional[3]` the JSON. `--help` returns
  the USAGE as a usageError ⇒ printed to stderr, **exit 2** (unlike exec/patterns —
  preserve this asymmetry).
- `list` (non-json): rows sorted by name; per row `<glyph> <name>  <bits · joined>`
  where bits = granted/not granted, `<toolCount> tools` if alive, `authed` if
  authorized, connection error if any; then a blank line + glyph legend
  `● connected · ◐ granted, not connected · ○ not granted` (pinned). Empty registry:
  `no MCP servers registered — bough mcp add NAME URL, or bough sync-mcp`, exit 0.
  `--json` dumps the whole McpStatus, indent 2.
- `doctor`: connects **sequentially on purpose** (parallel handshakes make slow servers
  look broken). Skips the connect round trip entirely when it is known to be refused:
  only servers that are `active` AND (remote OR a session is present) are connected
  (test pins zero `/connect` calls for an ungranted-or-local-without-session setup —
  actually: for granted local without session). `diagnose` order (first true wins):
  1. connected ⇒ ok, note `<n> tool(s)`.
  2. not in `active` ⇒ bad, `not granted — bough mcp grant <name>` (advice about the
     credential would be "advice about a step that has not been reached").
  3. local (`registry.servers[name].url` not a string) AND no session ⇒ **unknown**,
     `local command — not tested; needs a conversation: bough mcp doctor --session ID`.
     UNTESTED IS NOT BROKEN — unknowns do not flip the exit code.
  4. error matches `/expired at/` ⇒ bad, "its Claude Code grant expired — use that
     server in Claude Code once, or: bough mcp auth <name>".
  5. error matches `/has no string at/` ⇒ bad, "Claude Code's grant for it is empty —
     re-authorize it there, or authorize bough separately: …".
  6. remote AND `!auth[name].authorized` ⇒ bad, `no credential — bough mcp auth <name>`
     — the `remote` guard is load-bearing: `status.auth` exists only for url entries, so
     without it every stdio server was told to run an OAuth flow that cannot exist
     ("live for exactly one commit").
  7. else bad with the raw connect error.
  Output rows `✓|✗|? name  note`; summary line
  `all N tested servers working [· K not tested]` or `B of N need(s) attention [· K not tested]`.
  Exit 1 iff any row is `bad`. `--json` prints the rows array.
- `test NAME`: exit 2 on network fail; `--json` prints ConnectResult and exits
  0/1 by `connected`; human prints `✓ name connected · N tools` + indented
  comma-joined tool names, or stderr `✗ name did not connect — <error>` exit 1.
- `auth NAME`: POST auth; ≥400 ⇒ stderr + exit 1. `status=="authorized"` ⇒
  "<name> was already authorized". Else needs `authorizationUrl` (missing ⇒ exit 1
  "sent no URL"); if `correctedUrl` present print
  `note: its endpoint was corrected to <url>` (registry was rewritten en route). URL is
  **printed, never opened** ("used over SSH and in CI … shelling out to a browser hangs
  where there is none"): `open this to authorize <name>, then come back — it finishes on
  its own:` + `  <url>`. Poll: sleep(1000) then GET auth, until injected-now ≥ deadline;
  give up ⇒ stderr `still waiting on the browser after <timeout>s — run auth again`,
  exit 1. On authorized: **CONNECT, do not stop** ("storing tokens changes no observable
  state … a flow whose success is invisible reads as a flow that failed") — connect
  WITHOUT session; on success print `✓ <name> connected · N tool(s)` and then re-fetch
  status: if not in `active`, add `  not granted yet — bough mcp grant <name>`; exit 0.
  Authorized-but-unconnected ⇒ stderr + exit 1.
- `logout`: DELETE auth; success message states scope: "forgot bough's credentials for
  <name> — the registration is untouched".
- `grant`/`revoke`: POST enable/disable with `{"sessionId": ""}` (global). Messages:
  "<name> is granted in every conversation" / "<name> is revoked everywhere".
- `add`: PUT with `{url}`; success names the next steps: "<name> registered — bough mcp
  auth <name>, then grant it".
- `remove`: DELETE; "<name> removed, along with any grants it held".
- `call SERVER TOOL [JSON]`: args from positional else (if a stdin reader is wired)
  stdin trimmed (empty ⇒ no args `{}`); malformed JSON ⇒ exit 2 with guidance, **before
  any request is made** (test pins zero calls). Session query param from `--session`
  else `$BOUGH_SESSION` (exported into every shell a turn spawns — this is what makes
  the grant enforced be the calling turn's, "without the model … being trusted to report
  it honestly"). ≥400 ⇒ relay `errorOf` verbatim ("rewriting … would lose the part that
  resolves it"), exit 1. Success prints `result ?? null` only.
- Note for the binary port: the production stdin closure `new Response(Bun.stdin.stream()).text()`
  is only *invoked* when `call` has no argv JSON — reading stdin eagerly would hang every
  other verb.

### sync-mcp
- Exit: 0 synced or nothing to do; 1 = any `failed` result OR any source-read error
  (even when others landed — test: broken entry reported, good one still written, exit 1);
  2 usage. "No servers anywhere" with no read errors is exit 0 + `no MCP servers found
  in Claude Code's config — nothing to sync.`
- `--from/-C DIR` repeatable; default dirs = `[cwd]`. `-n/--dry-run`, `--force`,
  `--no-plugins`/`--plugins`, `-h/--help` (usage on stdout, exit 0). Any positional ⇒
  usage error ("a typo'd flag looks like it worked").
- Collection precedence: user file(s) → project scope per dir → `<dir>/.mcp.json`,
  later `take()` wins by name; config-dir `.claude.json` is read after `~/.claude.json`
  so it wins ("when it exists it is the live one"). Source labels are the actual paths
  with home shortened to `~` (plus ` projects[<dir>]` suffix) — "a person looking at two
  entries that disagree needs to know WHICH of the two files won." Per-entry schema
  failures are per-name errors ("<source>: <name> is not a server definition — skipped"),
  not aborts. `readJson` returns null for ENOENT (normal), throws for
  permission/parse errors (collected into `errors`, warned, and force exit ≥1).
- Plugin collection appended AFTER config files with config names winning ("someone who
  has written their own is saying they want theirs" — matched on the verbatim
  `plugin:x:y` name).
- Grants: read via `credentialReaderFor(holdsGrants)` — NOT the default reader; the
  predicate picks whichever store (login keychain vs `$CLAUDE_CONFIG_DIR/.credentials.json`)
  actually holds a non-empty `mcpOAuth` map. Empty-token grants warn per grant
  (`"<name>" holds no token — … Re-authorize it in Claude Code, or remove the server`);
  stale-but-nonempty grants get ONE aggregate note ("already expired. Adopted anyway —
  Claude Code refreshes its own tokens … bough does not refresh them"). Both are still
  adopted — the reference is written either way, silently was the bug.
- Grant matching: by Claude Code's name first, then by URL modulo trailing slashes
  (`sameUrl`). A grant with no definition anywhere (the Slack case) is itself a server:
  `{ type:"http", url }`, source "Claude Code's keychain grants" — skipped if its name
  or URL is already found (so it does not ALSO land as a second server).
- Identity & idempotency: `identityOf` = normalized URL, else `"<command> <args joined>"`
  trimmed, else null. Pre-existing same-identity duplicates get a warning naming both
  (`X and Y are the same server (<id>). Only one is needed. Open /mcp and press F on the
  one you do not want.`) — reported, never deleted. For each found server: the endpoint
  decides the target name FIRST — among existing entries sharing the identity, prefer
  the `natural` name (`boughName(claudeName, ∅)`) if that entry exists, else the first
  same-identity name; only then fall back to `boughName(claudeName, taken)`. This is
  what makes (a) a second run a no-op even for stdio plugin servers (first run's
  `mcp-search` would otherwise be "taken" and spawn `plugin-claude-mem-mcp-search`),
  (b) an adopted credential land on `slack` rather than a fresh `plugin-slack-slack`
  when both duplicates pre-exist.
- `boughName`: valid slug = `^[a-z0-9][a-z0-9_-]*$` — valid raw returned untouched;
  else prefer slugified LAST `:`-segment; if taken or unusable, slugify the whole
  (`plugin:slack:slack` → `plugin-slack-slack`); both taken ⇒ None ⇒ failed result
  `no free name could be derived from "<raw>"`. Renames recorded (`renamedFrom`) and
  said in output; renamed names are added to `taken`.
- Existing entry, no `--force`: normally `skipped` ("already registered here — --force
  replaces it", or `already registered as "<name>", the same server` when it matched via
  endpoint under another name). ONE exception: an existing **url** entry with NO
  non-empty `Authorization` header, when a grant exists for it, gets the header added in
  place (all other fields untouched) ⇒ `updated` with reason "added the missing
  credential" — "adding a header where there was none is not the clobber `--force`
  guards against". An entry that already carries any non-empty Authorization (even a
  `${VAR}` one) is left alone.
- `toBoughServer` credential ladder for remotes: existing Authorization header wins
  (authed=false, header kept as-is); else the server's OWN grant ref (authed=true);
  else account-token ref ONLY for Anthropic hosts (authed=true); else bare headers.
  Host check is suffix-after-dot or exact on `claude.ai`/`anthropic.com`, lowercased,
  URL-parsed — `claude.ai.evil.example` must NOT match ("a credential leak with a
  helpful tone of voice"). `oauth.clientId` carried into the entry as `clientId`
  (Slack publishes `registration_endpoint: null` — without it the entry is
  un-reauthorizable once the adopted grant expires).
- Per-entry expired-grant warning at write time (in addition to the aggregate note):
  "<name>: Claude Code's grant for this server expired <ISO>. bough does not refresh a
  credential it did not obtain — run `claude` once to refresh it in place."
- `looksSecret` env values warn: "<name>: env <K> looks like a literal secret. bough's
  registry is served by GET /mcp/servers — prefer ${K} and put the value in ~/.bough/env."
  Warning only, never a refusal.
- Report lines: `✓|· <name>[ (renamed from <x>)]  <action>[ — <reason> | (using the token
  Claude Code already holds)]   (<source>)`. `--dry-run` prints
  `--dry-run: N entr(y|ies) would change, nothing written.` and exits 0 always. When
  `wrote > 0` (and not dry-run): trailing "N server(s) registered. Registering grants
  nothing — open the /mcp panel and enable the ones a turn should be able to use." —
  said every time ("this is the step whose absence looks like a bug").

### tags
- Exit: 0 answered (incl. empty memory / empty results / `--help`); 1 = no DB file yet
  (`no command memory yet at <path> — run something through bough first`), embed layer
  absent, or `similar` failing; 2 = usage OR sql refusal OR sql driver error.
- Parsing: bare word ⇒ `show <word>` ("`bough tags git` is what a hand reaches for");
  `sql`/`similar` take exactly one (quoted) argument, stored in `tag`; `show` exactly
  one; `stats` zero; extra rest after an unknown first word ⇒ "unknown verb". `--limit`
  and `--days` positive, truncated to int. **`--all` beats `--repo` regardless of
  order** ("a correction, not a contradiction") — repo is deleted after the loop.
- Repo scope: `--all` ⇒ undefined (unscoped); else `--repo` value; else
  `workspaceRepo(cwd)`. Exception: the default `list` view is ALWAYS scoped to the
  checkout even under `--all` ("there is nothing meaningful to rank 'every project's
  tags' by").
- `list`: `rankedRepoTags(db, scope, now, limit)`; human table headers
  `tag/weight/repos/score` (weight `%.1f`, score `%.1f`), header line is the repo
  identity, empty ⇒ "no tagged commands yet — the model tags them as it runs them";
  footer explains the ranking ("ranked by weight × how FEW repos use the tag…").
  Pinned inversion: a tag heavier in-repo but used across repos must rank BELOW a
  this-repo-only tag. References (`linear.*`-style) never appear in the list but ARE
  recallable via `show` ("recalled but never primed").
- `show`: newest first; header `N command(s) tagged "<tag>"`; the full tag string
  printed only when it CHANGES between consecutive rows; per row
  `✓|·|✗ <ago padded 9> <cmd whitespace-collapsed, ≤96 chars>` (mark: 0 ⇒ ✓, null ⇒ ·,
  else ✗ — exit code first because "what worked here" is the question). Rows with a
  `messageId` get either the full program (`--program`, each line prefixed `      │ `)
  or a pointer `      ↳ program: N line(s) · --program to see it`. `--json` maps rows
  to `{...row, program: string|null}`. Empty ⇒ `no commands tagged "<tag>"`, exit 0.
- `stats`: window `now - days*86400e3`; columns
  `day/sessions/cmds/tagged/vocab/refs/uses/once` where tagged = `round(tagged/commands*100)%`
  (or `—` when commands==0) and once = `round(singletons/distinctTags*100)%` (or `—`);
  rows sliced to `limit` in the human view only; long explanatory footer (see source).
  `--json` prints the raw rows unsliced.
- `sql`: uses `dbFile ?? dbPath()` on a fresh readonly connection (NOT the injected db).
  Refusal and driver errors both append `. Queryable: <SURFACE>.` and exit 2; driver's
  own message relayed ("what lets them fix it"). Rows capped at 200, printed as JSON
  indent 2. Blocked by prefix check: DELETE/UPDATE/DROP/PRAGMA…; allowed: SELECT/WITH.
- `similar`: layer absent ⇒ exit 1 with a message that includes a full working
  `bough tags sql "SELECT … MATCH 'docker' …"` fallback example. Present ⇒ await
  `layer.similar(text)`, print ≤200 rows JSON, always `layer.close()` (finally).
- `ago()`: <90 s ⇒ `Ns ago`; <90 min ⇒ `Nm ago`; <48 h ⇒ `Nh ago`; else `Nd ago`
  (rounded at each step).
- Entry point MUST call `enableSqliteExtensions()` **before the first DB open** —
  a one-shot swap; it was missing here once and `similar` was "structurally dead on
  every machine: writes worked, reads could not."
- Help asymmetry: parse returns `{usageError: USAGE}` for `-h`; runTags prints it to
  **stderr** and exits 0 only when the string equals USAGE exactly.

---

## 5. Dependencies

Imports (cli → other bough modules):
- exec: `schema/parts` (Session, TurnStatus, AskQuestion), `schema/events`
  (MessageDeltaData, MessageRetryData, TurnFinishedData), `types` (UsageTotals),
  node `fs/promises.realpath`.
- patterns: `logs/analyze` (Analyzer), `logs/format` (toHuman/toJson/toLlm).
- mcp: `mcp/status` (McpStatus type only — pure HTTP client otherwise).
- sync_mcp: `mcp/config` (loadRegistry, upsertServer, McpConfigOptions),
  `mcp/keychain` (CLAUDE_CODE_ITEM, claudeConfigDir, credentialReaderFor,
  KeychainReader), `zod` (permissive schemas), node os/fs/path.
- tags: `bun:sqlite` (readonly connection), `db/db` (openDb),
  `db/extensions` (enableSqliteExtensions), `history/embed` (createEmbedLayer),
  `history/stats` (rankedRepoTags, workspaceRepo, RankedTag), `paths` (dbPath),
  `types` (Db, TagDiversityDay, TaggedCommand).

Imported by: nothing in-process — each module is a standalone entry point launched by
`scripts/bough`. (`tui/args.ts`, `mcp/keychain.ts`, `server/turns.ts` reference the
files only in comments.) Tests drive exec/mcp against the REAL `server/app.ts` handler
over an in-memory DB — the Rust port should preserve that seam (a `fetch`-shaped
function so tests can hand the router in directly, e.g. calling an axum `Router` via
`tower::Service::oneshot` without binding a socket).

---

## 6. External deps → Rust equivalents

| TS/Bun | Used for | Rust |
|---|---|---|
| global `fetch` / `Request` / `Response` | all HTTP | `reqwest` (client) behind a trait so tests inject `tower::Service`-backed fakes |
| `ReadableStream` + `TextDecoderStream` + `AbortController` | SSE consume + deadlines | `reqwest::Response::bytes_stream()` + `tokio::time::timeout` / `CancellationToken`; hand-rolled SSE framing (keep `createSseReader` as a pure fn — do NOT pull an SSE crate; the parser's drop-malformed/comment semantics are pinned) |
| `Bun.write(Bun.stdout)` / `console.log/error` | output | `std::io::Write` on locked stdout/stderr (writes are whole-string; no partial-write loop needed with `write_all`) |
| `Bun.stdin.text()` / `.stream()` | prompt/args/log input | `tokio::io::stdin` + `read_to_string`; for patterns `BufReader::lines` |
| `process.stdin.isTTY` / `stdout.isTTY` / `columns` | format & prompt decisions | `std::io::IsTerminal`; `terminal_size` crate for width |
| `node:fs/promises.realpath` | `--workspace` validation | `std::fs::canonicalize` (error ⇒ exit 2) |
| `bun:sqlite` | tags direct DB + readonly sql | `rusqlite` with `OpenFlags::SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=ON; busy_timeout=2000` |
| `zod` permissive schemas | Claude Code file parsing | `serde_json::Value` + hand validation, or serde structs with `#[serde(default)]` + `flatten`-into-`Map` for passthrough; per-entry failure must skip, not abort |
| `readFileSync` + ENOENT-as-null | sync-mcp readJson | `std::fs::read_to_string` mapping `ErrorKind::NotFound` to `Ok(None)` |
| `setTimeout`-based auth poll / sleep | mcp auth | injected `sleep(Duration)` + `now()` closures (tests: no real waiting) |
| `crypto.randomUUID` (tests only) | fixtures | `uuid` crate |
| argument parsing | all | keep the hand-rolled parsers verbatim — do NOT swap in clap; the exact error strings, `--flag=v`, dash-leading values, `--`, `-` sentinel, and per-command asymmetries (help→stdout-0 vs help→stderr-2) are pinned by tests |

Env vars read: `BOUGH_PORT`, `BOUGH_SESSION` (mcp call), `CLAUDE_CONFIG_DIR` (via
keychain helper), `BOUGH_HOME` (via paths, indirectly). All through injected lookups.

## 7. Suggested Rust layout

```
bough-cli/            (or a `cli` module in the main crate)
  mod.rs              subcommand dispatch (bough exec|mcp|sync-mcp|tags|patterns)
  exec.rs             ExecArgs, parse_exec_args, run_exec, ExecEnvelope, stop_turn
  sse.rs              SseFrame, SseReader (pure struct: push(&str) -> Vec<SseFrame>)
  patterns.rs         PatternArgs, parse_args, run_patterns (thin; logs crate does the work)
  mcp.rs              McpArgs, parse_mcp_args, run_mcp, diagnose (pure fn — unit-test it)
  sync_mcp.rs         SyncArgs, collect_claude_servers, collect_plugin_servers,
                      to_bough_server, bough_name, read_grants, run_sync_mcp
  tags.rs             TagsArgs, parse_tags_args, run_tags, query_sql, render_*
```

Traits / injection:
- One `HttpFn` boundary shared by exec+mcp: `Fn(Request) -> Future<Response>`
  (`Box<dyn Fn>` or generic). Tests pass the axum router's service; prod wraps reqwest.
- Deps as plain structs of closures/trait objects mirroring the TS `*Deps` interfaces
  (out/err as `FnMut(&str)`, clock as `Fn() -> u64` ms, sleep as async closure). Do not
  invent a global; the DI-over-globals rule is the tested surface.
- `run_*` all return `i32`; `main` maps to `std::process::exit` and is the only impure
  site. Async boundary: exec, mcp (auth poll, call), sync-mcp (keychain read is async in
  TS — a subprocess `security` call; in Rust use `tokio::process::Command`), tags
  (`similar` only) ⇒ make all `run_*` `async fn` under tokio, with patterns/sync-mcp
  internals mostly sync.
- Keep `diagnose`, `bough_name`, `identity_of`, `looks_secret`, `is_anthropic_host`,
  `holds_grants`, `ago`, the SSE parser and every arg parser as pure functions — they
  carry the majority of the pinned behavior and unit-test without any harness.

## 8. v1 scope cut

Core (needed for a working agent loop + daily driving):
- **exec**: full port, invariants intact — it is the headless surface (2026-07 priority)
  and the bench driver. Nothing in it is cuttable; the ordering, interrupt-on-timeout,
  ask-decline and retry-reset behaviors are each a shipped bug fix.
- **tui/args-level dispatch** of subcommands in the binary.

High (daily-driver, port early but after the loop closes):
- **mcp**: list/grant/revoke/add/remove/test/call are thin HTTP passthroughs — cheap.
  `doctor`'s diagnose table and `auth`'s poll loop are the only logic. Depends on the
  server routes existing in the Rust server first; until then the whole command can
  ship and simply exit 2 (`no bough server at …`).
- **tags** list/show/stats/sql: direct rusqlite reads; depends on the db/history schema
  port. `sql` is trivially portable and immediately useful for debugging the port.

Later:
- **sync-mcp**: interop with Claude Code's config/keychain — valuable but standalone
  (no server, no DB). Port after the mcp registry file format (`mcp/config`) is settled
  in Rust. Its test suite is the spec for the credential-safety invariants; do not port
  it without them.
- **patterns**: self-contained but drags the whole `src/logs/` clean-room pipeline
  (Analyzer/Drain clustering/formatters) with it — that is its own subsystem spec.

Stub in v1:
- `tags similar` (needs sqlite-vec + lembed extension loading — print the existing
  "no local embedding layer … Keyword search always works: bough tags sql …" message
  and exit 1; that IS the designed graceful-absence path).
- `patterns` may ship as a stub that prints "not yet ported" + exit 2, since nothing
  else calls it.
- exec `--json` usage/treeUsage enrichment can degrade gracefully (envelope without
  `usage`) until `GET /sessions/:id` reports usage in the Rust server — the contract
  explicitly allows the absence.
