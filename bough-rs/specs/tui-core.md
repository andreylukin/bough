# tui-core — port spec

Source: `src/tui/{api,args,clipboard,events,forest,format,keys,lines,main,mouse,paste,selection,store,term,theme}.ts(x)`
Scope: all TUI plumbing *below* the React/OpenTUI component layer. The logic is deliberately
renderer-free in the TS tree already ("pure functions of strings and data, correct with no
terminal attached"); a ratatui port reuses it nearly 1:1. `main.tsx` is the composition root
whose *responsibilities* (not its React code) must be reproduced.

---

## 1. Purpose & invariants

The TUI is a loopback HTTP + SSE client for the bough server (default `http://127.0.0.1:4321`,
port from `BOUGH_PORT`) plus a full-screen terminal frontend. Each module opens with a stated
invariant; these are the contracts the Rust port must preserve, quoted:

- **api.ts** — "no component talks HTTP, and no URL is written twice." Every server route
  reachable from the TUI is a method on one client; "the client never re-declares a wire shape
  it can import"; "everything is injected" (base URL + fetch fn), so tests point it at a fake.
- **args.ts** — "an unknown flag is an error." "Pure and total, so the whole surface is
  asserted without a terminal." A positional arg is refused with a hint toward `bough exec`.
- **clipboard.ts** — "THE ORDER IS THE WHOLE POINT": image data on the pasteboard beats the
  text beside it; a clipboard whose *text* is one absolute path to an image file is a picture
  too; everything else stays text.
- **events.ts** — "**`seq` is a dedupe key, not a resume cursor**." Nothing resumes: no
  `Last-Event-ID`, no remembered seq; on RE-connect the caller re-fetches `GET /sessions/:id`
  and reconciles by message id. "The known-type list is the schema's, never a local copy."
  "No React, no terminal, no globals" — fetch, retry delay and URL all injected. The envelope
  is the one place bytes come off a socket and IS schema-validated; malformed frames are
  skipped, never fatal.
- **forest.ts** — "ONE tree, for everything: conversations, the turns inside them, and what
  branched off which turn." Rules: (1) a conversation appears exactly once, under what it
  branched from; (2) turns are shown for expanded conversations only; (3) spawned work
  collapses into a count. "PURE. Rows in, rows out — no fetch, no clock, no React."
- **format.ts** — "every function here is a pure function of strings and data"; "**display
  width is never `String.length`**" (all measurement via string-width, all slicing via
  slice-ansi); "color is a display setting, not a parameter of the data" (module-level color
  state changes how a line is painted, never what it says).
- **keys.ts** — "there is exactly one description of what a key does, and it is the thing
  that makes the key do it" (bindings-as-data; help generated from the table). "Resolution is
  pure and needs no terminal." "The same chord may mean two things, and the guard says which."
  "The panel's tab list is part of the keymap." "A tab-local key says so in the table."
  "`key.super` is only believable under the kitty keyboard protocol."
- **lines.ts** — "the transcript is data before it is a component": `buildLines` produces one
  entry per physical row, pre-wrapped, pre-styled, each carrying click target and copy text.
  "Folding is decided by predicates the caller owns." "A running program is visible while it
  runs" (live `tool.log` lines render under a call with no result, and are *replaced*, not
  duplicated, when the `tool_result` lands).
- **mouse.ts** — "ink's input parser only ever receives keystrokes": mouse reports, bracketed
  paste, focus events and OSC replies are filtered out of stdin first. "The filter is a pure
  state machine over strings." "Only sequences that can actually split across reads are held"
  (never a bare ESC — that would swallow the Escape key). Home/End, Cmd+←/→, backtab,
  forward-delete and `CSI 27;m;k~` are intercepted here.
- **paste.ts** — long pastes leave a **mark in the draft** (`[Pasted text #N]`) instead of a
  remembered offset: edits cannot desynchronize it, deleting the mark drops the paste, order
  is the draft's order. An ordinal is a stable *name*, never a position.
- **selection.ts** — "selection is arithmetic on display columns, never on string indices";
  a selection is normalized (drag any direction); the release cell is inside the selection;
  the highlighted span drops its own colours (one solid inverse band).
- **store.ts** — "state lives here, rendering lives in components, and the reducer touches
  neither a terminal nor a server." "**The reducer is idempotent under re-delivery**" with
  three defence layers: (1) `seq:ts` dedupe window, (2) snapshot watermark (server persists
  THEN publishes, so events stamped before the snapshot request are already in it), (3)
  identity-keyed part appends. "A snapshot merges, it does not clobber."
- **term.ts** — "what a terminal can do is a pure function of its environment, and every
  decision that depends on it is taken here." Kitty protocol is *pushed* unconditionally but
  *trusted* only when detected. Effects behind an injected writer.
- **theme.ts** — "**browsing never commits**": preview paints the real palette; a held
  baseline is restored byte-for-byte on cancel; only `commit()` moves the baseline. "A theme
  is pure data." "One apply paints every path" (SGR params, component colors, screen bg).
- **main.tsx** — "the terminal is restored on every exit path"; "this file is the only place
  that knows the process exists"; "preflight fails with a sentence, not a stack trace"
  (exit 2); "the terminal has two writers, and the split is written down" (renderer owns
  `?1049`/cursor/DEC-2026 framing; bough owns title push/pop, SGR mouse modes, `?1004`).

---

## 2. Public API

### api.ts
- `DEFAULT_PORT = 4321`; `defaultBase(): String` — `http://127.0.0.1:${BOUGH_PORT ?? 4321}`.
- `class ApiError { status, message, method, path }` — non-2xx; message is the server's own
  `{error}` text when present, else trimmed body, else `"METHOD path: status"`.
- `class OfflineError { base, cause }` — fetch itself failed. Message text (load-bearing,
  truncation-ordered): `` `bough server unreachable — run: bough start · ${base}` ``.
- `createApi({base?, fetchFn?}) -> Api` — one `send` funnel; JSON bodies with
  `content-type: application/json`; `query()` omits null/undefined/"" params (no bare `?`);
  `seg()` percent-encodes path segments. Methods (route ⇢ return type):
  - `base`, `eventsUrl(sessionId?)`, `artifactUrl(sessionId, name)` (name split on `/`,
    each segment encoded).
  - Sessions: `listSessions(originId?)` ⇢ `SessionRow[]` (top level excludes collapsed
    kinds; with `originId` it is the drill-in); `createSession(body)`; `getSession(id)` ⇢
    `SessionSnapshot`; `patchSession(id, body)` (absent field = leave alone, explicit
    `null` = clear pin); `sessionUsage(id)` ⇢ `{usage, tree}`; `postMessage(id, body)` ⇢
    `{message, queued}` (202; `queued` = a turn was already running); `uploadImage(blob)`
    ⇢ `{path, mediaType, name, size}` (raw body POST /attachments, content-type = image
    mime); `putDraft(id, draft|null)`; `interrupt(id)` ⇢ `InterruptResult` (always resolves
    for an existing session; `{interrupted:false}` when the turn had already ended).
  - Asks: `listQuestions(sessionId?)`; `answerQuestion(sid,qid,answer)`;
    `declineQuestion(sid,qid)` (`{decline:true}` body).
  - Jobs: `listJobs(id)` ⇢ `{jobs: JobListRow[]}`; `runShell(id, command)` ⇢
    `{id,name,pid}`; `killJob(id,jobId)`; `jobOutput(id,jobId)` ⇢ `{output, job}`
    (non-destructive — never moves the model's cursor).
  - Changes: `getChanges(id)` ⇢ `SessionChangeSet` (always 200; "not a repository" is an
    answer); `revertChanges(id, paths?)` — **omitting `paths` reverts everything; an
    explicit empty array is a server 400**, never a wildcard.
  - History: `fork`, `unsend` (deletes the named message + everything after, stops the turn,
    returns text; only the session's own LAST user message is accepted), `compact`,
    `sections` (stateless: gists in, labeled ranges out), `extract`, `moveInto`, `handoff`.
  - Workflows: `listWorkflows(sessionId?)`, `createWorkflow`, `getWorkflow` (only place
    `replay` is guaranteed), `stopWorkflow`, `pauseWorkflow`, `resumeWorkflow`,
    `rerunWorkflow`, `relaunchWorkflow`, `workflowReplay`, `controlWorkflowAgent(id,
    agentId, "stop"|"restart")`, `saveWorkflowAs`, `listSavedWorkflows`, `getSavedWorkflow`,
    `putSavedWorkflow`, `runSavedWorkflow`.
  - Models: `getModels()` ⇢ `ModelCatalog` (asked of the server because the *server* holds
    the key); `getModelSettings()`, `putModelSettings({model?, effort?})`.
  - Workflow settings: `getWorkflowSettings`, `putWorkflowSettings(sizeGuideline)`.
  - Schedules: `listSchedules`, `createSchedule`, `patchSchedule`, `deleteSchedule`.
  - Artifacts/comments: `listArtifacts`, `listComments`, `postComment`, `deleteComment`,
    `sendComments(id, ids?)`.
  - MCP: `mcpStatus(sessionId?)` (never cached by callers), `putMcpRegistry`,
    `putMcpServer`, `deleteMcpServer`, `connectMcpServer(name, sessionId)` (`""` = process
    scope; connect proves, does not grant), `restartMcpServer`, `setMcpEnabled(name, on,
    sessionId, ttl?)` (`""` = global scope), `mcpAuthStatus`, `beginMcpAuth` (returns URL,
    never opens a browser), `clearMcpAuth`.
  - Search: `search(q, {sessionId?, limit?})`, `reindex()`.
  - Theme: `getTheme()` (never 404s), `putTheme`, `deleteTheme` (what the "Default" preset
    means) — all return the same `ThemeState` document.
  - Ghost: `ghostText(sessionId, prefix)` ⇢ `{ghost: string|null}` — **always resolves**;
    `{ghost:null}` covers every failure; the composer must render no error path.
  - Files: `listFiles(sessionId)`, `listFilesIn(workspace)`, `listDirEntries(dir, base?)`,
    `branch(dir)` ⇢ `{branch}`, `listSkills()` ⇢ `{skills, sources}` (fresh disk walk per
    call).

### args.ts
- `USAGE: string` (verbatim text incl. "programs run as you, with your authority — there is
  no sandbox" and the explanation that port is env-only).
- `parseTuiArgs(argv) -> TuiArgs | {usageError} | {help:true}`; `isTuiUsageError`,
  `isTuiHelpRequest`. Supports `-w DIR`, `--workspace DIR`, `--workspace=DIR`, `-w=DIR`,
  `-h/--help`. Empty/whitespace workspace value ⇒ usage error. Unknown flag ⇒ usage error
  including USAGE. Positional ⇒ error suggesting `bough exec "<token>"`.

### clipboard.ts
- `type Clipboard = {image: Blob} | {text: string} | null`.
- `clipboardImagePath(text) -> {path, mediaType} | null` — pure; trims; rejects multi-line
  and non-absolute; accepts `file://` URLs (unparseable ⇒ null); extension map:
  png/jpg/jpeg/gif/webp ⇒ image/{png,jpeg,jpeg,gif,webp}.
- `clipboardFromText(text) -> Clipboard` — reads the named file; a missing/unreadable file or
  non-file falls back to `{text}` (never an error).

### events.ts
- `KNOWN_EVENT_TYPES: Set<string>` — imported from frozen `schema/events.ts` `EVENT_TYPES`.
- `RETRY_MS = 2000`.
- `parseFrames(buffer, emit) -> tail` — pure SSE framing: frames end `\n\n`; `event:` sets
  type; multiple `data:` lines concatenate (one optional leading space stripped per line);
  comments (`:`)/`retry:` ignored; emits only when both type and data are non-empty; returns
  unconsumed tail.
- `connectEvents(options) -> EventStream {connected, opens, close(), done}` — infinite loop:
  fetch with `accept: text/event-stream`; on non-ok/absent body: cancel body, throw, retry;
  `connected` true only after headers land, cleared on every exit incl. clean EOF; `onOpen({
  reconnect: opens>1, attempt})`; unknown-type / non-JSON / schema-invalid frames go to
  `onBadFrame` and are skipped; `onClose({error})` then abortable delay `retryMs`; `close()`
  aborts, `done` resolves when the loop stops. Delay resolves early (not rejects) on abort.

### forest.ts
- Re-exports `COLLAPSED_KINDS`, `DELEGATED_KINDS`; `isCollapsed(kind)`, `isDelegated(kind)`
  — collapse ⊃ delegate; a schedule firing collapses but is NOT delegated.
- `type ForestRow = {kind:"session", id, session, depth, open, delegated, current,
  busyBelow, expandable} | {kind:"section", id:"section:<sid>:<i>", sessionId, depth, label}
  | {kind:"message", id, sessionId, depth, role, gist, matched?, active, last} |
  {kind:"collapsed", id:"collapsed:<sid>", originId, depth, count}`.
- `messageText(m)` — text parts joined verbatim (take-back must not gist).
- `messageGist(m, max=56)` — whitespace collapsed; empty ⇒ `"(N steps)"` from tool_call
  count or `"(no text)"`; truncates to `max-1` + `…`.
- `forestRows(input: ForestInput) -> ForestRow[]` — the walk (see §4).
- `revealPath(sessions, childrenByOrigin, currentId) -> string[]` — origin chain root-first,
  excluding self, cycle-guarded. Seeds expansion only.
- `rowAt(rows, selected)`; `rewindIndex(rows, currentId)` — last USER turn, else last turn,
  else own session row, else 0.
- `type Selection = {none:true} | {open: id} | {expand: id} | {drill: id} |
  {fork: {sessionId, atMessageId, exclusive?}, editorText?}`.
- `selectionFor(row, threads)` — collapsed ⇒ drill; section ⇒ none (⏎ on a caption must not
  fork at a nonexistent message id); session ⇒ open; user message ⇒ fork exclusive with
  `editorText = messageGist(m, Infinity)`; other message ⇒ inclusive fork.
- `type TakeBack = {kind:"queued"} | {kind:"sent", atMessageId, text} | {kind:"none"}`;
  `takeBackTarget(queued, thread)` — queued first; else last *user* message searched from
  the end (the reply may already be streaming); text via `messageText`.

### format.ts (selected exports; all pure)
- Color state: `colorEnabled()`, `setColorEnabled(on) -> was` (init from `NO_COLOR` empty);
  `colors: ColorParams` (SGR *parameter bodies*, e.g. `muted:"38;5;245"`,
  `surfaceBg:"48;5;236"`); `setColors(partial)`; `UI: UiColors` (component color names, ANSI
  floor `green/yellow/red/cyan/gray`); `setUiColors(partial)`.
- Styling: `fg(params,s)` closes with `39m` (never `0m` — a full reset strips the base row
  color); `bold`(1/22), `underline`(4/24), `italic`(3/23), `strike`(9/29), `dim`, `accent`,
  `warn`, `danger`, `info`.
- Measurement: `width(text)` (display cols, escapes 0, CJK 2); `truncateAnsi(text, max,
  ellipsis="")` — binary search over visible chars, escapes kept and closed, ellipsis charged
  to budget; `wrapLine(text, max)` — hard wrap, trim:false (keeps code indentation), clamps at
  `MIN_WRAP = 20`.
- ANSI→structure: `AnsiSpan {text, fg?, bg?, bold?, dim?, italic?, underline?, reverse?,
  strikethrough?, link?}`; `ansiSpans(text)` — spans concatenate to exactly
  `strip_ansi(text)`; SGR 0 clears style but an OSC 8 link is not SGR state and survives it;
  256-color (`38;5;n` via xterm cube/ramp/BASE16) and truecolor (`38;2;r;g;b`) resolved to
  `#rrggbb`; adjacent same-style runs merge. (ratatui: this is the bridge from styled strings
  to `Span`s.)
- Text helpers: `clip(s,n)` (char count + `…`); `oneLine(s)` — control bytes⇒space, newlines⇒
  `" ¶ "` (deliberate: the join must be visible), tabs⇒space, runs collapsed, trimmed;
  `codeGist(input, max=60)`; `plural(n, word, plural?)`; `legendLine(items, width?)` — drops
  middle items with `…`, **the LAST item (escape hatch) always survives**; `windowAround(
  selected, total, height)` — centered clamped slice, no padding for short lists.
- Part folding: `Segment` union (`text|reasoning|image|ask|workflow|tools`); `segmentParts`
  — consecutive tool_call/tool_result fold into ONE `tools` group; prose splits groups; ask
  and workflow stand alone. `outputText(r)`; `toolSummary(parts)` ⇢ `{calls, results(Map by
  callId), running, hasError, errors, interrupted}`.
- `programSummary(code, max=64, running=false)` — heuristic label of what a program did
  (see §4 for the regex traps); returns `""` when nothing is recognized (caller falls back
  to codeGist).
- Markdown-lite: `md(text, codeWidth?)` — line-by-line except GH tables (gathered, laid out
  as padded columns, widest column shrunk to fit, no tables inside fences); fences framed
  `╭ lang` / `│ …highlighted…` / `╰` and optionally `surface`d; `#`-headings bold (h1
  underlined); `---` ⇒ dim 24-char rule; `> q` ⇒ dim `│ q`; `- ` ⇒ `• `; inline: code spans,
  `**bold**`, `~~strike~~`, `*i*`/`_i_` (underscore only at word boundaries — dunders and
  snake_case survive; delimiter must hug text), `[text](url)` ⇒ OSC 8 with dim `(url)` unless
  label==url, bare URLs linkified (guard placeholders `\x00N\x00` prevent double-processing).
- `highlightCode(line, langTag)` — one-pass string/keyword/number tokenizer for
  js/python/go/rust/bash/sql (alias map; unknown ⇒ js "C-family"); line comment split
  outside quotes. "Candy, not a parser."
- `surface(line, w)` — paint `surfaceBg` behind a padded line; internal `\x1b[0m` re-opens
  the bg.
- Links: `osc8(url,text)`; `urlAt(plain, col)` (bare URL under a column; trailing
  `.,;:!?` stripped); `urlAcross(rows, row, col)` — rejoin a URL wrapped across content rows;
  rows join when the upper ends and lower begins on URL-chars and the lower has no space;
  scans backward first (clicks land mid-URL); `linkAt(text, col)` — OSC 8 target under a
  display column; `linkifyUrls(line)` for raw output.
- Numbers: `fmtTokens` (`1.2k`), `fmtUsd` (sub-dollar keeps digits), `CTX_WARN_PCT = 20`,
  `ctxPctLeft({contextTokens, contextLimit}) -> pct|null` (null when limit unknown —
  never invent a percentage), `meterLine(opts)` — the bottom status row with a degradation
  ladder (see §4), `coldCacheNote(usage, now)` (`❄ re-caches ~Nk` iff ≥20k ctx tokens and
  lastLlmAt older than 5-min TTL), `relTime`, `DISCONNECT_ESCALATE_MS = 15000`,
  `disconnectNote(sinceMs, now)` ⇢ quiet "reconnecting…" then urgent named-elapsed text,
  `sessionLabel(title, workspace)` (title unless empty/"untitled", else workspace basename,
  else "(untitled)"), `humanizeRetryReason(raw, max=60)` (status-code names + nested JSON
  `"message"` lifted; never classifies unknown errors), `shortenPath(path, home)`,
  `SPINNER`/`SPINNER_MS=120`, `busyLine({activity, elapsedMs, tick, tokens?})` — always
  motion + elapsed + `esc interrupts`; tokens but deliberately NOT cost;
  `unitLine(u: LiveUnit, cols)` — rail row: glyph (⚙ shell warn / ◆ subagent info / ⏱
  schedule dim / ⧉ workflow accent), schedule counts DOWN (`in 40s`/`due`), detail dropped
  when it repeats the title (compared against UNCLIPPED title) or room < 8;
  `fmtDuration` (`9s`, `1m04s`, `1h02m`).
- Completion: `fuzzyScore` (prefix 4 > boundary(-_ /␣) 3 > substring 2 > subsequence 1 > 0),
  `fuzzyPositions` (same tier order), `Trigger {kind:"file"|"skill", query, start, end}`,
  `activeTrigger(text, cursor)` — `/` and `@` fire at position 0 or after whitespace only;
  whitespace between marker and cursor ⇒ finished, not completing; `end` = next whitespace
  after cursor (token replaced whole); `browsePrefix(query)` — `~`, `/`, `./`, `../` leave
  the workspace (returns literal prefix up to last `/`); bare `src/` stays on git;
  `Completion {label, detail, insert, run?, hl?}`, `rankCompletions(candidates, trigger,
  limit=6)` ⇢ `{items, total}` — shorter-name tiebreak only when a query was typed (else
  source order so built-ins lead); insert = `marker+name+" "` (no space if name ends `/`);
  hl positions shifted +1 for the marker; `applyCompletion(text, trigger, item)`.
- Word motion: `wordLeft`, `wordRight` (whitespace-delimited readline rules).

### keys.ts
- `type UiMode = "chat" | "rail" | "ask" | "panel" | "help" | "job"`.
- `type Command` — full union (~80 commands; see source list — port verbatim as an enum).
- `TABS` (7 rows: tree ^f, changes ^d, workflows ^w, model ^o, mcp ^p, skills ^k, theme ^y,
  each with title + desc); `PanelTab`, `TabCommand = "tab.<id>"`; `PANEL_TABS`;
  `PANEL_TOGGLE = "ctrl+t"`; `SESSIONS_ALIAS = ctrl+s ⇒ tab.tree` (documented);
  `tabForChord`, `tabForCommand`.
- `SLASH_COMMANDS` — TABS-derived rows + new/compact(takesArg)/rewind/schedules/saved/
  artifacts/rules/help; `SlashCommand {name, command, desc, takesArg?}`.
- `slashCommandFor(draft)` — exact whole-draft `/name` only; `slashInvocation(draft)` —
  adds `/name arg…` for takesArg commands; `unknownCommand(draft, skills)` — lone `/word`
  that is neither command nor skill ⇒ `{name, suggestion}` via `FOREIGN_COMMANDS`
  (clear/reset⇒new, resume/sessions/history⇒tree, cost/status⇒model, diff⇒changes,
  exit/quit⇒none) then prefix/containment over commands+skills. Never silently aliases.
- `KeyFlags` (structural subset of ink's Key incl. `super`); `chordOf(input, key) -> String`
  — canonical `"ctrl+p"`, `"meta+enter"`, `"esc"`, `"?"`; `""` for non-chords (pastes,
  coalesced chunks, bare modifiers); bare `"\n"` with no return flag ⇒ `"ctrl+j"` always;
  backspace and delete flags both ⇒ `backspace`; shift only recorded on enter/tab.
- `chordLabel(chord)` — glyph table (ctrl⇒^, meta⇒⌥, super⇒⌘, shift⇒⇧, arrows, ⏎, esc, ⇥,
  ⌫, pgup/pgdn, space).
- `KeyContext {mode, tab?, emptyDraft, inSubagent?, multiline, busy, justSent?, doubleEsc,
  quitArmed, railLive, hasAttachments?, completing, panelFiltering?}` — every optional flag
  absent = safe degrade (documented per field). `Guard` = boolean keys minus mode/tab.
- `Binding {mode|"*", chord, command, when?, not?, tab?, section?, desc?, label?}`;
  `BINDINGS: Binding[]` — resolution order matters only to put guarded rows before their
  unguarded fallback. `UNSEND_MS = 3000`. `FILTER_TABS = ["tree","model","skills"]`.
- `lookup(ctx, chord) -> Command|null` (first matching row whose guards hold; tab-scoped
  rows dead when `ctx.tab` null or outside set); `resolve(ctx, input, key)`.
- Help: `HelpSection {section, keys:[[chord,desc]], limits?, unavailable?, commands?}`;
  `LIMITS` ("won't do" prose), `UNAVAILABLE` ("not bound": ^r, ^z, ⌥d, home/end);
  `helpSections()` — generated from BINDINGS; `when:["emptyDraft"]` appends
  `" · empty draft"`, `not:["emptyDraft"]` appends `" · with a draft"`; plus "typed at the
  prompt" (`!cmd`, `@path`, `/`-commands) and "marks in the tree" (● ↦ ⑂ ≣ ◆, ⋯ ✓ ✗ ◼)
  sections; `HelpLine {kind:"header"|"row"|"blank", chord, desc, muted?, prose?}`;
  `helpLines(sections)` — flat physical rows so the overlay is a slice (a squashed flexbox
  once destroyed every header); `deadBindings(bindings)` — shadow detection incl. tab-set
  subset/superset rules.
- Line editing: `LineState {text, cursor}` (cursor is a **char index into the string**;
  byte-vs-char matters in Rust — use char indices or a rope); `EMPTY_LINE`;
  `editLine(state, command) -> LineState` — returns the SAME object on no-ops (Rust: return
  unchanged/bool); commands: cursor.left/right/home/end/wordLeft/wordRight/up/down (home/end
  are the *logical line's*; up/down keep the column, stop at ends), delete.back/forward/
  wordBack/toEnd/toStart/line, newline; `insertText(s, text)`;
  `stripCtl(s)` — SS3 (`ESC O <char>`) first, then strip-ansi (CSI/OSC), then any remaining
  `ESC <printable>` pair, then C0 bytes except `\n`/`\t`… (regex: `[\x00-\x08\x0b-\x1f\x7f]`)
  — **whole sequences, never just the ESC byte** (else `⌥⏎` types `[27;3;13~` into the
  draft); `chunkInput(chunk) -> {body, send}` — only a *trailing* `\r` sends; `\r\n?`
  normalized to `\n`; a bare `\n` is always literal (^j); `isTextInput(input, key)`.

### lines.ts
- `VLine {text, click?, copy?, src?}` — click targets: tool-group key `"<msgId>:<segIdx>"`
  toggles fold, `"<key>!full"` lifts the block cap, `"open:<sessionId>"` opens a subagent,
  `"report:<sid>"`/`"report:<sid>!full"`, `"job:<sessionId>:<jobId>"`,
  `"workflow:<runId>"`, `"<msgId>:workflow"`. `src` = the single unwrapped logical line
  (copy-across-wrap rejoins; deduped by the reader).
- Caps: `CODE_LINES=14`, `OUTPUT_LINES=20`, `REPORT_LINES=6`. Expand-all must NOT lift them.
- Note parsers (regex over system-message text; keep byte-exact):
  `parseSubagentNote(text) -> SubagentNote {title, sessionId, status, ok, files,
  filesUnknown, report}` — header `[subagent finished] "T" (sid) — status.`; only
  `finished*` is ok; `Changed files: …` where `not reported` ⇒ filesUnknown, `none` ⇒ [];
  report between `Report:\n` and `\nIt worked in THIS session's checkout`.
  `parseBgNote` — `[background] <id>( "name")? finished` (name optional for old rows).
  `parseImageNote` — `[image] <path>( — note)?`.
  `parseWorkflowNote` — `[workflow done|error|stopped] "name" (id) — …` with optional
  leading `N/M agents succeeded.`.
  `splitMarginNotes(text) -> {body, hints}` — trailing `[history] `/`[rules] ` lines walked
  backwards in any order; model-facing ` — …` tails cut; `[history]` rewritten
  `"<dir> also remembers: a · b"`.
- `messageLines(msg, isExpanded, isFull, w, streaming?, toolLogs?, runs?, now)` — system
  image note collapses to one row; workflow completion note ⇒ fold card; else blank +
  role label (`you` bold / `bough` bold accent / `system` bold amber) + segments indented
  two columns; streaming text appended with `▌` cursor.
- `Branch` + `branchesFrom(thread, children)` — DELEGATED children only (forks/handoffs/
  compactions must not dress up as subagent reports); note matched by sessionId; status only
  from the settled set; card drawn at `originMessageId`.
- `JobView extends BackgroundJob {tail?, outputLines?}`; `jobCardLines` — glyph color by
  state; **a signalled job is `◼ stopped (SIG)`, never `✓ done`** (exitCode null + `?? 0`
  was the bug); command not repeated when it equals the name; card click opens the job.
- `RunCardView` + `workflowCardLines` — part carries identity only; counts/status/elapsed
  read live from the run row; `done` with failed>0 ⇒ `⚠ done`, not ✓.
- `skillsNamed(text, installed)` — `/name` tokens at start/whitespace, matched against the
  installed list (lowercased), deduped, in order.
- `buildLines(thread, isExpanded, isFull, w, opts: BuildOptions) -> VLine[]` — see §4.
- `chatBodyHeight(height, queued, hasNotice) = max(1, height - (queued + 2 + notice?1:0))`;
  `lineAtSlot(lines, body, scrollOff, slot)` — inverse of the render loop incl. top padding
  (short conversations hang from the BOTTOM); `visibleSlice(lines, height, scrollOff)` ⇢
  `{start, rows, more, pct}` — scrollOff counts up from the live tail.

### mouse.ts
- `MouseEvent {x, y (1-based cells), kind: down|drag|up|right-click|wheel-up|wheel-down}`.
- `NavKey = home|end|cmdHome|cmdEnd|shiftTab|forwardDelete`.
- `InputSinks {mouse?, paste?, navKey?, focus?, bgReport?}`.
- `decodeModifyOther(mods, code) -> String` — `CSI 27;mods;code~`; mods is a 1-based
  bitfield (bit0 shift, 1 alt, 2 ctrl, 3 super); ctrl folds letters to C0; alt prefixes ESC;
  undecodable ⇒ `""` (swallowed, never typed).
- `createInputFilter(sinks) -> {feed(chunk) -> forwarded}` — pure state machine: bracketed
  paste (`ESC[200~`…`ESC[201~`) accumulated across reads, delivered whole with `\r\n?→\n`;
  SGR mouse `ESC[<b;x;yM/m` (64/65 wheel; bit32+btn0 drag; btn0 down; btn2 right-click;
  `m`+btn0 up); focus `ESC[I/O`; OSC 11 reply; forward-delete `ESC[3~`; backtab `ESC[Z`;
  cmd-arrows `ESC[1;9C/D` ⇒ cmdEnd/cmdHome; nav Home/End (`[H|F`, `O[HF]`, `[1~`/`[4~`);
  `CSI 27` decoded last (kitty shift+tab `27;2;9~` ⇒ shiftTab nav key, not a bare tab).
  `PARTIAL_TAIL` holds only *incomplete-by-construction* trailing fragments
  (`\x1b\[(<[\d;]*|20[01]?|1(;9?)?|2(7(;\d*(;\d*)?)?)?|3)$`) — never a bare ESC.
- `filteredStdin(sinks)` — binds filter to process stdin, latin1 both ways (single code
  units; UTF-8 survives the round trip). Rust: same idea over raw bytes.
- `enterTui()` writes `CSI 22;0t` (title push) + `?1000h ?1002h ?1006h ?2004h ?1004h`.
  **NOT `?1049h`** — the renderer owns the alternate screen. `leaveTui(cleanup?)` —
  cleanup() swallowed, then `?1004l ?2004l ?1006l ?1002l ?1000l ?25h` + `CSI 23;0t`;
  idempotent; must run after renderer teardown (renderer blanks the title on exit).

### paste.ts
- `QUEUE_ABOVE_CHARS = 50` (deliberately low); `pasteMark(ordinal) = "[Pasted text #N]"`;
  `expandPastes(text, pastes)` — global regex replace; index = ordinal-1; unknown ordinal
  left verbatim; a paste with no remaining mark is dropped (that IS the removal gesture).

### selection.ts
- `Point {x,y}` 1-based; `Selection {anchor, focus}`; `selRows`, `isEmptySelection`
  (click ≠ drag), `rowSpan(sel, y) -> {from, to}|null` — 0-based display columns, `to ===
  Infinity` = end-of-line; interior rows whole; end rows clip to drag cells; release cell
  inclusive (`to = b.x`).
- `highlightSpan(text, from, to)` — `\x1b[7m mid \x1b[27m`, mid stripped of its own colors;
  empty mid ⇒ untouched row. `extractSpan` — stripped, trimEnd.
- `rowContent(text) -> {content, offset}` — strips right border (`\s*[│╮╯]\s*$`) and left
  chrome (`^(\s*)[│╭╰]\s?`); offset = columns removed on the LEFT (click hit-testing).
- `selectedCopy(sel, rowAt: y -> CopyRow|null)` — single row: exact span, chrome stripped.
  Multi-row: a run of rows sharing one `src` yields that src ONCE (unwrap + un-gutter via
  `cleanSource`) **only when the drag covers the whole source** — every row edge-to-edge
  (`coversRow`) and no row of the same src one past either end; otherwise cells. Gaps paste
  as blank lines. Fence-only rows contribute nothing (vs. genuinely empty src ⇒ ""). Sources
  are stripped of SGR (highlighting happened before wrapping).
- `selectedText(sel, rowAt)` — literal cell reading; null rows ⇒ blank line; trailing blank
  lines from padding popped.

### store.ts
- Constants: `DEDUPE_WINDOW=256`, `RECONCILED_LIMIT=64`, `MARK_LIMIT=500`,
  `NOTICE_TTL_MS=10_000`, `USAGE_POLL_MS=3_000`.
- `TuiSessionRow = SessionRow & {unseen?}` (client-only). `TranscriptMark {id, sessionId,
  at, kind:"destructive"|"turn", text}` — never expires, survives session switches.
  `TurnMeter {sessionId, startedAt, baseTokens, baseCostUsd, tokens, costUsd, endedAt,
  status}` — turn delta measured from session totals at start.
- `TuiState` — full shape in source (connected, sessions, currentId, session, thread,
  streaming: msgId→text, toolLogs: callId→lines, asks, queued, lastSendAt, notice, activity,
  usage, effectiveModel, contextLimit, primedTags, projectRules, droppedIds (tombstones),
  changes, jobs, jobView, workflows, schedules (GLOBAL — survives switches), workflowLogs,
  workflowSeq, replay, background {sessionId,title,seq}, marks, turn, reconciledAt, seen).
- `initialState()`; `StoreAction` union (event, connection, sessions, open, snapshot{at:
  fetch-ISSUED time}, questions, ask.settled, changes, jobs, jobView, workflows, schedules,
  replay, notice, mark, effectiveModel, usage, turn.settle, queue, queue.drained, queue.pop,
  sent, thread.dropped).
- Pure primitives: `eventKey({seq,ts}) = "seq:ts"`; `isDuplicate(state, event)`; `partKey`
  (tool_call:id, tool_result:callId, ask:id, image:path, workflow:id; text/reasoning ⇒ null
  — legal to repeat, never content-deduped); `mergeThread(fromDb, local)` — union by id,
  longer part list wins, `pending = fromDb.pending && local.pending`, stream-only messages
  appended; `totalTokens(u) = input+output+reasoning` (cache tokens already inside input —
  do not add twice); `settledLine(turn, endedAt)` — `✓|✗|⏹|⚠ 14s · 3.2k tok [· interrupted|
  failed]`, zero tokens omitted; `reduce(state, action)` (see §4);
  `isBusy(state)` = any pending message; `currentAsk(state, descendants)` — hold belongs iff
  its session, walked up `originId ?? parentId` (cycle-guarded), reaches currentId or a
  descendant; `liveText`; `marksFor`; `LiveUnit {kind, id, sessionId, title, elapsedMs,
  tokens, costUsd, progress, detail}`; `liveUnits({jobs, subagents, workflows, schedules,
  now})` — running shells (by startedAt) then busy subagents (by createdAt) then
  running|paused workflows then enabled schedules (elapsedMs = nextRunAt - now, unclamped);
  workflow progress = (done+cached)/total; details oneLine'd (one rail row = one screen row,
  always).
- `createStore({api?, connect?, now?}) -> Store` — the I/O shell: getState/subscribe/
  dispatch/start/stop/reload/open/createSession/newConversation/compact/runShell/
  searchSessions/describeSchedules/describeSavedWorkflows/describeArtifacts/
  describeProjectRules/send/drainQueue/answerAsk/declineAsk/interrupt/takeBackQueued/
  unsend/stopUnit/setModel/refreshChanges/refreshUsage/refreshJobs/openJob/refreshJob/
  closeJob/refreshWorkflows/refreshReplay/resync/notify/record/dismissNotice. Details §4.

### term.ts
- `termCaps(env) -> TermCaps {program, term, tmux, zellij, kitty, progress, tabColor,
  notify}` — kitty iff not tmux AND (TERM_PROGRAM ∈ {ghostty, WezTerm, iTerm.app, rio} or
  TERM ∈ {xterm-kitty, foot, foot-extra} or KITTY_WINDOW_ID set); progress iff TERM_PROGRAM
  ∈ {ghostty, iTerm.app, WezTerm} (kitty renders OSC 9 as a NOTIFICATION — never send it
  progress); tabColor iff iTerm.app and not tmux; notify = "bell" for Apple_Terminal else
  "osc9".
- `kittyKeyboardMode() = "enabled"` always — push unconditionally (auto-probe dies in
  tmux); the caps flag governs *trust* of `super`, not the push.
- `sanitize(text)` (control bytes ⇒ space); `tmuxWrap(seq, inTmux)` — `ESC Ptmux;` DCS with
  every ESC doubled; `parseBgSpec("rgb:xx/xx/xx")` — 1–4 hex digits per channel scaled to
  8-bit ⇒ `#rrggbb`; `classifyBg(hex)` — Rec. 709 luma < 128 ⇒ dark;
  `TITLE_SPINNER` (10 braille frames); `boughTitle(session, status, frame)` —
  `"bough · label · ⠋|complete"` filtered-joined.
- `createTerm({caps, write, renameTmuxWindow?, renameZellijTab?, timers…}) -> Term`:
  `setTitle` (OSC 0 + tmux window rename + zellij tab rename via CLIs — best effort);
  `notifyDesktop(body)` — ONLY while unfocused; bell or tmux-wrapped OSC 9;
  `progressStart` — OSC `9;4;3` re-asserted every 5s (Ghostty expires ~15s);
  `progressEnd(error)` — clear, or error state `9;4;2;100` cleared after 4s;
  `tabColor(hex|null)` — iTerm2 `OSC 6;1;bg;…` triple, `*;default` reset;
  `osc52Copy(text)` — base64, payload capped at 72_000 bytes, NOT tmux-wrapped (tmux
  translates OSC 52 itself); `queryTermBg()` — OSC `11;?` (reply arrives on stdin via the
  filter); `reportTermBg(spec)` — malformed never clobbers a good value; `termBackground()`;
  `setFocused/isFocused`; `cleanup()` — clear progress + tab tint + timers.
- `term()` — lazy process-wide instance (import must not touch env/stdout);
  `syncedStdout()` — DEC 2026 wrap per write (obsolete under OpenTUI; ratatui: unnecessary
  if the backend frames writes, else wrap each frame in `?2026h`…`?2026l`);
  `terminalSize()` — measured each call, clamps (min 20×8, fallback 80×24);
  `onResize(handler)` — SIGWINCH, returns unsubscribe, failure ⇒ static size.

### theme.ts
- `ThemeColors = map<token, hex>`; `ThemeState {theme: {name, colors}|null, defaults}`.
- `FALLBACK` — complete contrast-checked palette (green #4ec98f, amber #d9b45f, red
  #e2776e, blue #5c88c9, hairline #666d79, bg #0e1013, panel #14161a, panelInset #1f2329,
  text #e7e9ed, text2 #c9cdd4, muted #9aa1ac, muted2 #7a828e — AA at 4.91:1).
- `palette: TuiPalette` — mutable singleton {accent, warn, error, info, border, bg, panel,
  panelInset, text, text2, muted, muted2, epoch}; `resolveColors(state)` =
  FALLBACK ← defaults ← theme.colors.
- `subscribeTheme(fn)`, `themeEpoch()` — the change-notification pair (React bailed out on
  unchanged state; ratatui equivalent: a redraw flag).
- `setBackgroundPainter(fn|null)` — paints on registration too (boot applies theme before
  the renderer exists); `applyTheme(state)` — writes palette + `setColors` (truecolor SGR
  params via `fgParams`/`bgParams`) + `setUiColors` + background painter, bumps epoch,
  notifies listeners LAST (copy the set — listeners may unsubscribe while running).
- `fgParams(hex) = "38;2;r;g;b"` (3-digit hex expanded); `bgParams` swaps 38→48.
- `THEME_PRESETS` (Default={}, Fjord, Iris, Ember (+amber move), Rosewood (+red move),
  Lagoon, Graphite, Midnight (surfaces), Rosé Pine Moon (full palette)); accent/warn/error
  must stay three distinguishable hues. `presetSwatch(p)` — resolved from the preset's OWN
  colors, never the live palette. `presetIndex(state)` (-1 for custom).
  `stateFor(base, preset)` — empty partial ⇒ `{theme:null}` (the reset ⇒ DELETE /theme).
- `createThemePreview({current?, apply?, persist?}) -> ThemePreview {presets, index,
  previewing, name, move(delta) (clamped, never wraps), select(i), commit(), cancel()}` —
  cancel idempotent, restores baseline; commit moves baseline then fire-and-forgets
  persist (a failed save must not unpaint the screen).

### main.tsx (composition-root responsibilities)
- Parse argv BEFORE taking the screen (usage errors print to the normal buffer; help ⇒ exit
  0, usage error ⇒ exit 2). `preflight()` — one `listSessions`; failure prints
  `bough tui: <OfflineError message>` and exits 2.
- Default workspace: `-w` else `BOUGH_TUI_CWD` else cwd.
- Fetch theme AND model catalog BEFORE the first frame, both best-effort (fallbacks:
  FALLBACK palette; compiled-in MODELS list — an empty picker looks broken).
- `openUrl` — **http(s) only, a security boundary**: transcript URLs are model-written;
  `open`/`xdg-open`, detached, failures ignored.
- Clipboard image path (macOS): try pasteboard PNG via a compiled-on-first-use Swift helper
  (`~/.bough/bin/pasteboard-png`, TIFF→PNG); fall back to `pbpaste` text →
  `clipboardFromText`. Non-darwin ⇒ null.
- Single-listener hubs for paste/mouse/navKey (two registered handlers would double
  keystrokes); stdin filter wired with focus→term.setFocused, bgReport→term.reportTermBg.
- Terminal ownership split (see §1 quote); `enterTui()` before renderer,
  `leaveTui(cleanup)` in finally AND on process exit (idempotent); `queryTermBg()` only
  after raw mode is on (the reply must hit the filter).
- Tab title: derived from store state (`boughTitle`), spinner ticks at 120ms only while the
  open session's turn runs; title written only on change.
- persistTheme: `theme === null` ⇒ DELETE (never PUT an empty map — it would store a named
  theme overriding nothing).
- Exit: stop spinner, unsubscribe, `store.stop()`, `leaveTui`, then hard process exit (the
  stdin listener would hold the loop open).

---

## 3. Data structures

### Wire shapes (exact field names; all JSON over loopback HTTP)
- `SessionRow = Session & {busy: bool, lastTurnStatus?: TurnStatus, costUsd?: f64,
  tokens?: u64}` — extras derived server-side; optional fields absent from older servers
  must degrade, not break.
- `SessionSnapshot {session, thread: Message[], usage: UsageTotals & {tree: UsageTotals},
  effectiveModel?: String, contextLimit?: u64|null, primedTags?: String[],
  projectRules?: ProjectRuleSummary[]}`; `ProjectRuleSummary {label, path, bytes}`.
- `SessionUsage {usage, tree}`; `ModelSettings {defaultModel, cheapModel: String|null,
  defaultEffort: Effort|null}`; `PostedMessage {message, queued}`; `BranchResult {session,
  thread, turnStarted?}`; `MoveResult {session, thread, appended}`;
  `JobListRow = BackgroundJob & {tail?: String[], outputLines?: u64}`;
  `JobOutput {output, job}`.
- `WorkflowSummary {id, name, description, status, currentPhase: String|null, phases,
  agents: {total, done, cached, running, queued, failed}, result, error: String|null,
  resumeOf: String|null, createdAt, finishedAt: i64|null, scriptFile}`;
  `WorkflowDetail {workflow, agents, scriptFile, live, replay, cost, warning, guideline}`;
  `RerunResult = WorkflowRun & {replay}`; `RelaunchResult {workflow, source, script,
  replay}`; `ReplayReport = RelaunchReport & {line}`; `WorkflowSettings {sizeGuideline,
  target, advice, tokenWarnThreshold, concurrency?, maxAgentsPerRun?, advisory: true}`.
- `McpConnectResult = McpStatus & {server, connected, error?, tools: [{name,
  description}]}`; `ReindexResult {rebuilt, messages, sessions}`.
- `ThemeState` (above). Attachment upload response `{path, mediaType, name, size}`.
- SSE envelope: `BoughEvent {type, seq: u64, ts: i64 (ms), sessionId?, data}` —
  schema-validated at the socket; event types (the frozen closed set from
  `schema/events.ts`): `session.created`, `session.updated`, `session.activity`,
  `message.started`, `message.delta {messageId, delta}`, `message.retry {messageId,
  attempt, reason}`, `message.part {messageId, part}`, `message.finished {messageId}`,
  `turn.finished {sessionId, status}`, `tool.log {callId, line}`, `ask.question`,
  `job.spawned`, `job.exited`, `workflow.updated`, `workflow.agent`,
  `workflow.log {runId, line}`. The Rust port must derive this list from the shared schema
  crate, never keep a local copy.
- `Message {id, sessionId, role: "user"|"supervisor"|"system", parts: Part[], pending,
  createdAt, …}`; `Part` union: text{text}, reasoning{text}, image{path, name, size,
  mediaType}, ask{id, question, status: pending|answered|declined|interrupted, answer?},
  workflow{id, name, description, rerunOf?}, tool_call{id, name, input}, tool_result{callId,
  output: String|json, isError, interrupted}.

### Client-only state
`TuiState` / `TranscriptMark` / `TurnMeter` / `TuiSessionRow` / `JobViewState` /
`LiveUnit` / `VLine` / `ForestRow` / `Selection` / `LineState` / `Trigger` / `Completion`
— see §2. No DB tables are touched by this subsystem; the server owns SQLite.

---

## 4. Behaviors & edge cases (a naive port gets these wrong)

### SSE + reconnect (events.ts + store.ts) — the core protocol
1. Never resume. On reconnect (`opens > 1`) the shell calls `resync()`: reload sessions +
   asks, then snapshot the open session, then changes/jobs/workflows/schedules.
2. Dedupe layer 1: key is `seq + ":" + ts` — `seq` alone resets on server restart; `ts`
   alone collides at ms resolution. Window is 256, oldest evicted. An event is remembered
   even when it changed nothing ("seen is about delivery, not effect").
3. Layer 2: watermark = time the snapshot fetch was ISSUED (not landed) — conservative end,
   so an event published during the round trip is re-applied (and deduped) rather than
   dropped. Session-scoped events with `ts < watermark` dropped wholesale. Un-scoped events
   are never watermark-dropped. Watermark map capped at 64; the just-written and the OPEN
   session's entries are never evicted; a snapshot that lost the race with a session switch
   still records its watermark.
4. Layer 3: parts append only if no identity twin exists; text/reasoning parts have NO
   identity and are legal to repeat (never content-dedupe them).
5. Snapshot merge: union by message id; longer part list wins; pending only goes
   true→false; stream-only local messages survive; tombstoned (`droppedIds`) messages are
   filtered out of the incoming thread FIRST (a stale snapshot must not resurrect an
   unsent message); streaming buffers kept only for messages still pending in the merged
   thread.

### Reducer specifics
- `session.created`: skip if known; skip **collapsed kinds** (subagents/workflow agents/
  schedule firings never enter the top-level list); new rows get `busy:false`, prepended.
- `message.started`: `pending` marks the row busy; if it's the open session, merge-or-append
  to thread; a pending message in the open session IS the turn starting (no turn.started
  event) — TurnMeter created with `startedAt = event.ts` (never a wall clock in the
  reducer), base = current usage totals.
- `message.delta`: appended to `streaming[messageId]`; `message.retry` DROPS the partial
  (the re-stream is a competing copy, not a continuation) and notices with
  `humanizeRetryReason` when it's the open session.
- `message.part`: text part clears that message's streaming buffer; tool_result frees
  `toolLogs[callId]` (the result carries the same lines joined — retention depends on this).
- `message.finished`: clears busy; sets `unseen` on non-open rows; raises the `background`
  toast only for a previously-busy, non-open, NON-collapsed session (a subagent finishing
  raises none — its spawner's turn is still going); seq bumps to make repeats distinct;
  clears activity for the open session; marks the message not-pending.
- `turn.finished`: stamps `turn.endedAt`/status (not settled yet); marks row not busy with
  lastTurnStatus. The shell then: refreshChanges always; if open session, snapshot →
  (fallback refreshUsage) → `turn.settle` **after** the refetch (a settled line that
  under-reports the last round is worse than late); else `reload()` (a background session's
  cost would otherwise stay stale/zero). `turn.settle` writes a `kind:"turn"` mark with
  `settledLine`; ignores a turn that hasn't ended.
- `ask.question`: pending upserts (ordered oldest-first); settled removes.
- `job.spawned/exited`: patch known rows anywhere; only the open session's unknown rows are
  inserted (lineage rules live server-side).
- `open` (session switch): clears thread/streaming/toolLogs/queued/lastSendAt/activity/
  usage/effectiveModel/contextLimit/primedTags/projectRules/droppedIds/changes/jobs/
  jobView/workflows/workflowLogs/replay/turn; clears the target row's `unseen`; **marks and
  schedules survive**.
- `thread.dropped` (take-back): removes named ids (the server decided which — never
  re-derive the tail locally), tombstones them, drops their streaming buffers, disarms
  `lastSendAt`.
- Retention invariants (retention.test.ts): every container bounded — toolLogs freed on
  result-landing and on session switch; workflowLogs cleared with the session; seen ≤ 256;
  reconciledAt ≤ 64; marks ≤ 500; listeners leak-free over subscribe/unsubscribe cycles.

### Store shell
- Timers are armed off STATE TRANSITIONS, not call sites: notice expiry (10s, single TTL
  for every notice) re-arms whenever `state.notice` changes; usage poll (3s) runs exactly
  while `turn !== null && endedAt === null`. Both timers must not hold the process open
  (unref; in Rust: tokio tasks cancelled on stop).
- `record(msg)` = notice + permanent mark in ONE call (the seam that stops "the reasonable
  half" being done alone). A failed kill is a notice and NO mark (nothing was destroyed).
- `stopUnit`: shell⇒killJob, subagent⇒interrupt(sessionId), schedule⇒patch enabled:false
  (disable, not delete), workflow⇒stopWorkflow; then `record(...)` past-tense with scope;
  then targeted refresh.
- `send`: arms `sent` BEFORE the post (the window is about letting go, not the ack);
  queue-flag + busy ⇒ local queue; drainQueue posts in order once the turn ends and only
  into its own session (queue cleared on switch).
- `unsend`: server refusal is surfaced (its sentence is a real answer), local drop applied
  from `result.removed`, then an authoritative snapshot (failure silent).
- `setModel`: putModelSettings (install default) FIRST, then patchSession (open session);
  with nothing open and a model given, dispatch `effectiveModel` directly (no
  session.updated will arrive).
- `searchSessions`: hits inside collapsed sessions are attributed to `originId` (the
  spawner IS the row); returns both session ids and message ids; `[]` on failure (degraded
  search beats a modal error).
- `settleAsk`: optimistic removal, then the request; on failure re-read `/questions`
  (memory-only server-side — the server is the only one that knows).
- `compact`: refuses an empty thread; default goal string
  `"continue this work from where it stands, keeping whatever is still needed"`; opens the
  NEW root and returns the draft for the composer (a handoff, not a branch you stay in);
  success notice must name `^t` (composer-owned `^f` is guarded on empty draft and a
  handoff always lands with a draft).
- `createSession`: no title unless the caller has one (cheap tier titles from first
  message; `runShell`'s implicit session passes the command as title).

### forest walk
- `childrenOf` merges `sessions` (rows whose originId matches) with `childrenByOrigin`
  fetches, deduped by id. `seen` set is a CYCLE GUARD (originId is a pointer, not an FK) —
  malformed lineage renders a short tree, never hangs.
- `busyBelow` counts running descendants at any depth (`busy || lastTurnStatus ===
  "running"`), cycle-guarded — a collapsed row must not read `✓ done` over five working
  subagents.
- Roots = sessions with no origin OR an origin not in the list, filtered (`matches`: open
  conversation always survives; `matchedSessions` from full-text search survive; else
  title+workspace substring), sorted newest-first. Branches within a session sort
  oldest-first (rows must not move under the cursor).
- `expandable` is true when the thread is UNFETCHED (undefined ≠ empty), or has turns, or
  has branches/delegated children.
- Section headers: only at `sec.start === i` and only when the label has a letter
  (`/[a-z]/i` — the LLM really returns `…` labels); counted as ordinary rows by window math.
- Branch placement: under the message with matching `originMessageId`; a branch whose
  origin turn is not in the thread walks at depth+1 after the turns (still reachable).
  `userOnly` filters shown turns to role==user. `last` is true only for the final shown
  turn with no branches under it.
- Delegated children: one `collapsed` row with count unless `drilled` has the session, then
  each child walks at depth+1.

### keymap resolution (the ONE-PANEL keymap)
- Escape unwinds exactly ONE level, nearest surface first — resolution order in chat:
  completion popup → take-back (`justSent && emptyDraft`, 3s window) → interrupt (`busy`)
  → cancel. The double-esc rows: `draft.clear` (doubleEsc, NOT emptyDraft — an empty-draft
  double-tap must fall through to the stop, not be swallowed) and `tree.rewind` (doubleEsc,
  emptyDraft, not busy/completing).
- The take-back outranks the stop INSIDE the window because unsend stops the turn anyway;
  gated on emptyDraft so a draft is never traded for the sent message.
- `^c` is two rows: `quit` when `quitArmed`, else `quit.arm` — in mode `"*"`. A single ^c
  must never tear the UI down.
- Composer-owned chords (`^f ^d ^w ^k`) double as tab jumps guarded on `emptyDraft`; the
  other tab chords are unguarded. Tab chords bound in chat, panel, rail, AND ask (a held
  ask() must not swallow panel chords — the workflow approval card names `^w` itself).
  `^s` aliases `tab.tree` in all four modes and is documented.
- `↑` in chat: multiline ⇒ cursor.up; emptyDraft+hasAttachments ⇒ attachment.up; else
  history.prev. `↓`: multiline ⇒ cursor.down; attachments; emptyDraft+railLive ⇒
  rail.enter; else history.next. `←`: emptyDraft+inSubagent ⇒ session.out; else
  cursor.left.
- Panel: `j/k` and digits and every tab-local letter are guarded `not:["panelFiltering"]`
  (while the filter buffer is open, a letter is text). `⇥` = filterTier in model tab while
  filtering, else panel.next; `⇧⇥` = panel.prev. Filter esc: filterExit before
  panel.close. Tab-local letters: mcp {a,n,F,c,r,d}, workflows {p,P,x,r,e,s,f,o}, tree
  {s = confirmSummarize, e = extract, m = moveInto}, changes {x,X}. Disjoint tab sets on
  one chord are the design (`x` = wf.stop vs changes.revert); `deadBindings` verifies.
- `job` mode: esc/q/← close (back to the RAIL, not chat), ↑↓jk scroll, pgup/pgdn,
  `x x` kills.
- `chordOf`: multi-char input (paste/coalesced typing) ⇒ `""` (text, never matched);
  `chunkInput`: only a trailing `\r` sends (fast typists' Return arrives inside the chunk;
  a bare `\n` is ^j and always a literal newline — the OLD tree sent half messages).
- Slash dispatch runs at SEND time (`slashCommandFor` / `slashInvocation`) because pastes
  never open the popup — otherwise `/model` reaches the frontier model as prose (measured:
  19k tokens billed). Unknown lone `/word` is intercepted (`unknownCommand`) — `/clear`
  once reached the model, which invented a confirmation for a destructive op.

### transcript assembly (buildLines)
- Order: `#` margin rows (primedTags then projectRules, width-elided single rows) → thread
  in order, with marks and *exited* job cards flushed by timestamp BEFORE each message →
  per-message: skip subagent notes whose card renders (`notedIds`), skip bg notes whose job
  card shows; skill-loaded row under user messages; `⧖ queued` row under user messages
  posted mid-turn (after any pending message rendered); branch cards after their origin
  message → flush remaining marks → orphan branch cards under a
  "subagents with no spawn point in this thread" caption → remaining exited jobs →
  running job cards last (no exit time; they belong at the bottom).
- A running branch with no note is dropped (it lives in the rail); a note whose session is
  not among children yields no Branch — the raw note then still renders (never both, never
  neither).
- toolGroup header state precedence: running ⚙ → declined (hasError+declined) `⏹ declined`
  → partial errors `⚠ N of M failed` → all-error `✗ error` → interrupted `⏹ interrupted` →
  failed-commands `⚠ N commands failed` (non-error results whose output matches
  `\[exit code \d+`) → "". Names column lists only GRANTED tools (`run_steps`, `stop`) and
  only when >1 distinct. Collapsed gists: per call `programSummary || codeGist`; a
  non-granted call reads `called <name> as a tool`; live calls use present tense; repeats
  collapse to `… ×N`; one gist rides the header, several become indented step rows all
  carrying the fold's click key. Header row is never wrapped (one click target).
- Expanded call: label = programSummary(code)||name; input block (js-highlighted, cap 14,
  raised); result: `↳ output` + capped block (20) with `splitMarginNotes` hints rendered as
  `# …` marginalia after the block; a call with NO result renders `↳ output (live)` from
  toolLogs — an `else`, never an addition (the result replaces the live lines).
- `withoutStopSentinel`: strip a trailing `<stop>` (optionally fenced) the model wrote as
  prose — end-of-message only, fence must contain nothing else.
- Reasoning: empty ⇒ nothing at all; collapsed ⇒ one gist line; expanded ⇒ capped gutter
  block. Ask: one always-visible `? question → answer|declined|interrupted` line, never
  folded. Image: `🖼 name (N KB)` placeholder, copy = path.
- Scroll model: `scrollOff` counts UP from the live tail; `visibleSlice` clamps; short
  transcripts bottom-hang (top padding), and `lineAtSlot` must model that padding or every
  click is off by the pad.

### programSummary regex traps (all shipped bugs — port the guards)
`(?<![.\w])` not `\b` (else `.join(`/`.write(` member calls match host verbs); `${…}` in a
captured path ⇒ unnamed (count instead — never print `wrote ${cartPath}`); `patch(` is
counted but its file names come only from `[path#hash]` tags in the body; `join` destructured
from `node:path` shadows the host verb (both `import{join}from"path"` spellings); bare
`spawn(`/`exec(` are delegation/excluded from shell counts; `bashBg` ≠ `bash`;
`workflow.status()` polls read "checked the workflow run" not source; when nothing is
recognized return `""` so callers fall back to the code gist, never an empty header.

### meterLine degradation ladder
Full: `workspace[@branch] · model[ · effort] · $cost · ctx · shells · agents · runs ·
← back · ? help`. Context chip: unknown limit ⇒ `Nk ctx`; ≤20% ⇒
`⚠ N% ctx left — /compact` (the ONLY overflow warning — bough has no auto-compaction).
When too wide, degrade in fixed candidate order (basename workspace → drop workspace →
compact glyph live counts `⚙2 ◆1 ⧉1` → …) never wrap; `out`/`help` ride down the ladder
(the full line always starts with an absolute path, so on 100 cols it ALWAYS degrades —
anything added only to `full` ships invisible). Final fallback truncateAnsi + `…`.

### selection/copy
Single-row drags are EXACT (span, not source). Multi-row: substitute a shared `src` only
when the drag covers every row of it edge-to-edge AND the rows one past each end don't
share it. `cleanSource` strips SGR (sources are highlighted pre-wrap), drops fence-only
lines (≠ genuinely empty lines), strips gutter, trims trailing blanks; all-fence ⇒ null
(skip) vs empty ⇒ "" (blank line). Panel rows lose the right border too (`│` at line end)
— the mcp auth URL was the visible victim.

### input filter
Held tails must be incomplete by construction — the old tree held a COMPLETE cmd-arrow and
delivered it one keystroke late. A drag mid-paste must corrupt neither. Kitty shift+tab is
`CSI 27;2;9~` and must become the shiftTab nav key, not a bare tab. Backspace on macOS is
`\x7f` (ink `key.delete`) — forward-delete is ONLY `CSI 3~`.

### terminal effects
OSC 9;4 to kitty pops a NOTIFICATION banner every 5s — the progress gate is not cosmetic.
tmux passthrough-wraps everything OSC except OSC 52 (tmux translates it when set-clipboard
is on). Notifications fire only while unfocused. Ghostty expires progress ~15s ⇒ 5s
keep-alive. `queryTermBg` only after raw mode + filter are live. The error progress state
auto-clears after 4s.

### theme
Preview must repaint the actual UI (ratatui: applyTheme mutates the palette and sets a
redraw flag; render reads the palette each frame — the epoch/subscribe machinery exists
because React couldn't see singleton mutation). Cancel restores the ENTERED baseline; a
committed preset becomes the new baseline. Persist is write-behind and failure-silent.
"Default" persists as DELETE.

---

## 5. Dependencies

Imports (all type-only unless noted): `schema/parts.ts` (Message/Part/Session/kinds —
runtime: `isCollapsedKind`/`isDelegatedKind`, `EVENT_TYPES`, `BoughEvent` zod schema),
`schema/requests.ts` (request bodies), `schema/events.ts`, `types.ts` (Effort,
UsageTotals), and type-only imports from server/workflow/mcp/hostfn/history modules for
response shapes (deliberately erased — no runtime edge into the server; the Rust port puts
all wire types in a shared `bough-schema` crate used by both server and TUI).
`llm/client.ts` MODELS is imported by main only (composition root).

Internal graph (runtime): `format` is a leaf (type-only peek at store's LiveUnit);
`keys` → format; `lines` → format, forest, store(types); `forest` → api(types), schema;
`events` → schema; `store` → api, events, format, schema; `theme` → format;
`selection`/`paste`/`clipboard`/`args`/`term`/`mouse` are leaves; `main` → everything +
components. Imported by: `src/tui/components/*` (out of scope here) and `cli` entry.

---

## 6. External deps → Rust equivalents

| TS dep | Used for | Rust |
|---|---|---|
| `fetch` (Bun) | HTTP client | `reqwest` (or `hyper` direct; loopback only, no TLS) |
| SSE via fetch streaming | event stream | `reqwest` bytes stream + own `parseFrames` port (keep it hand-rolled; it's ~30 lines and the tests pin it) — or `eventsource-stream` |
| zod (`BoughEvent.safeParse`) | envelope validation | `serde` + `serde_json` with `#[serde(tag="type")]`, unknown types ⇒ skip |
| `string-width` | display columns | `unicode-width` (`UnicodeWidthStr`) — note: must skip ANSI first |
| `slice-ansi` / `strip-ansi` / `wrap-ansi` | ANSI-aware slice/strip/wrap | no drop-in; port `ansiSpans` first and do width/slice/wrap **over parsed spans** (strongly recommended over regex reimplementation — `ansi-str`/`console::strip_ansi_codes` cover strip only, and `textwrap` is not ANSI-aware). OSC 8 must be zero-width in all three. |
| Ink/OpenTUI | renderer | `ratatui` + `crossterm`. Crossterm delivers mouse/paste/focus events natively — most of `mouse.ts`'s *parsing* is replaced, but its **dispatch decisions** (which sequences become NavKeys, modifyOtherKeys decode, kitty shift+tab) must be re-verified; enable `crossterm` kitty keyboard flags (`PushKeyboardEnhancementFlags`) unconditionally per `kittyKeyboardMode`. |
| `node:process` env/stdout/SIGWINCH | env + tty | `std::env`, `crossterm::terminal::size`, `tokio::signal::unix::SignalKind::window_change` |
| `Bun.spawn` (tmux/zellij rename, open/xdg-open, pbpaste, swiftc) | side processes | `tokio::process::Command` (detached, output ignored) |
| `node:fs/promises` | clipboard file read, swift helper | `tokio::fs` |
| Blob upload | image POST | `reqwest::Body::from(bytes)` with content-type header |
| timers (`setTimeout`/`setInterval` + unref) | notice TTL, polls, spinner, progress keep-alive | `tokio::time::{sleep, interval}` in tasks owned by the store/term structs; abort on drop |
| base64 (OSC 52) | clipboard | `base64` crate |
| macOS pasteboard image | ⌘v image | keep the swiftc-on-first-use trick, or `arboard` (reads image data cross-platform — likely better) |

---

## 7. Suggested Rust layout

Crate `bough-tui` (bin) + reuse `bough-schema` (shared wire types + event enum).

```
bough-tui/src/
  api.rs          // Api struct: base + reqwest::Client; ApiError/OfflineError (thiserror).
                  // trait ApiClient (async_trait) so the store tests use a fake.
  args.rs         // parseTuiArgs port; no clap (the grammar is 4 tokens and USAGE is product text).
  events.rs       // parse_frames (pure, unit-tested) + connect_events -> tokio task feeding
                  // an mpsc<StoreAction>; handle {connected(), opens(), close()} via watch/AtomicBool + CancellationToken.
  store/
    state.rs      // TuiState + all client-only types.
    reduce.rs     // pure reduce(); dedupe/watermark/partKey/mergeThread; exhaustive match on event enum
                  // (no default arm — a new event type must be a compile error, mirroring the TS comment).
    shell.rs      // Store: owns state (single-threaded in the event loop; no locks needed if the
                  // TUI runs one task), api handle, timers (notice TTL, usage poll) as spawned tasks
                  // driven off state transitions; all the async verbs (send/unsend/stopUnit/…).
    selectors.rs  // isBusy, currentAsk, liveUnits, marksFor, settledLine, totalTokens.
  forest.rs       // forestRows/revealPath/rewindIndex/selectionFor/takeBackTarget (pure).
  ansi.rs         // AnsiSpan + ansiSpans + width/truncate/wrap/slice over spans; xterm256 table.
                  // THE bridge to ratatui: spans_to_ratatui(Vec<AnsiSpan>) -> Line<'_>.
  format.rs       // everything else in format.ts: styling fns emit ANSI strings exactly as TS
                  // (keeps lines.ts port 1:1 and copy/selection math identical), md/table/highlight,
                  // meterLine/busyLine/unitLine, fuzzy/trigger/completions, urlAt/urlAcross/linkAt.
  keys.rs         // Command enum, TABS const, BINDINGS as &[Binding], chord_of/lookup/resolve,
                  // help generation, deadBindings (as a #[test]), LineState editing, stripCtl/chunkInput.
                  // chord_of takes a normalized KeyPress built from crossterm::event::KeyEvent.
  lines.rs        // VLine, note parsers, buildLines + geometry (chatBodyHeight/lineAtSlot/visibleSlice).
  selection.rs    // Point/Selection/rowSpan/highlightSpan/extractSpan/selectedCopy/selectedText.
  paste.rs        // marks + expandPastes.
  clipboard.rs    // clipboardImagePath (pure) + async clipboardFromText; macOS image path in main.
  term.rs         // TermCaps (pure fn of env map) + Term (write: Box<dyn Fn(&str)> injected),
                  // parseBgSpec/classifyBg/boughTitle/tmuxWrap; timers as tokio tasks.
  input.rs        // what remains of mouse.ts after crossterm: NavKey mapping, modifyOtherKeys decode
                  // if crossterm misses any, enter/leave sequences NOT covered by crossterm
                  // (title push/pop CSI 22/23;0t, ?1004 if not using crossterm focus events —
                  // prefer crossterm's EnableFocusChange/EnableBracketedPaste/EnableMouseCapture).
  theme.rs        // palette (plain struct owned by the app, passed &mut — no singleton needed in Rust),
                  // presets, resolve/apply (apply also rewrites format's color params — make
                  // ColorParams a field of a RenderCtx passed into format fns, or an RwLock'd static
                  // to keep signatures identical to TS), ThemePreview controller.
  main.rs         // composition root: argv → preflight → fetch theme+models → terminal setup
                  // (crossterm alternate screen/raw mode; restore via Drop guard + panic hook),
                  // event loop: tokio::select! over crossterm EventStream / store actions / tick.
```

Traits & async boundaries:
- `ApiClient` trait (async) — the one seam tests fake. `connect_events` takes the client or
  a URL + a `Fn` for fetch.
- The reducer and every `format`/`forest`/`lines`/`selection`/`keys` function stay **sync
  and pure** — this is the property the entire TS test suite (200+ cases, names in §4/§2)
  pins; port those tests with the code.
- One tokio runtime; UI loop single-task; SSE task and timer tasks send `StoreAction` over
  an mpsc so `reduce` stays single-threaded (mirrors dispatch()).
- Terminal restore: an RAII guard (Drop) + `std::panic::set_hook` replaces the
  `process.on("exit")` + finally pair; must be idempotent and swallow its own errors.

---

## 8. v1 scope cut

**Core (must have for a working loop):** api (sessions/messages/interrupt/snapshot/usage/
events subset), events, store (reducer + dedupe + snapshot merge + queue + turn meter),
keys (chat mode + panel skeleton + line editing + chunkInput/stripCtl), format
(width/wrap/truncate/ansiSpans/md-basic/busyLine/meterLine), lines (messages, tool folds,
live toolLogs, visibleSlice), args, term (caps + title + enter/leave), main loop.

**Stub or drop initially:**
- **Drop:** workflows entirely (api methods, panel tab, run cards, replay, workflow events
  ⇒ reduce to no-ops), MCP tab + all mcp api, schedules (rail rows + describeSchedules),
  saved workflows, artifacts/comments, ghost text (`{ghost:null}` behavior means dropping
  it is invisible by design), sections/topic headers in the tree, `programSummary` (fall
  back to codeGist — the TS code does this anyway when nothing matches), search
  (`searchSessions` ⇒ `[]` is the documented degraded mode), image paste + attachments +
  swift helper, `urlAcross`/click-to-open (keep OSC 8 emission — cheap), tab tint /
  progress / desktop notifications (keep setTitle), theme presets + preview (ship FALLBACK
  palette; applyTheme plumbing can wait), zellij/tmux renames.
- **Stub:** mouse selection/copy (wheel scroll only at first; selection.rs ports cleanly
  later because it's pure), `unknownCommand` foreign-command suggestions (keep
  slashCommandFor — that one prevents billed `/model` messages), fuzzy completion popup
  (keep `@`-file trigger detection so typed text isn't mangled), job view mode (rail row +
  kill is enough; `jobOutput` view later), handoff/compact, unsend take-back (keep
  interrupt-on-esc; the 3s window is polish), history ops (fork/extract/moveInto) behind
  the tree.
- **Never cut:** the dedupe/watermark/merge trio (the reconnect story IS the product),
  `chunkInput`'s trailing-`\r` rule, `stripCtl`, the escape-unwind order, collapsed-kind
  filtering of the top-level list, OfflineError's sentence + exit 2, terminal restore on
  every exit path.
