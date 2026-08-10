# Port spec: `src/tui/components/` → Rust (ratatui)

Subsystem: **tui-components** — every screen/widget of the bough TUI. React/OpenTUI in TS;
target is ratatui widgets driven by an event loop. This spec describes LAYOUT and BEHAVIOR,
not React mechanics. Companion subsystems (specced separately, heavily referenced here):
`tui/lines.ts` (transcript building), `tui/keys.ts` (keymap), `tui/store.ts` (state + SSE),
`tui/format.ts` (ANSI/width/wording helpers), `tui/forest.ts` (tree fold), `tui/theme.ts`
(palette), `tui/mouse.ts` (stdin filter), `tui/api.ts` (REST client), `tui/selection.ts`
(drag-select math), `tui/paste.ts` (held pastes).

Files covered (all under `src/tui/components/`): `App.tsx` (2546), `PanelHost.tsx` (2043),
`Workflows.tsx` (752), `ModelPicker.tsx` (469), `Composer.tsx` (390), `Panel.tsx` (390),
`Changes.tsx` (386), `Tree.tsx` (362), `Mcp.tsx` (246), `Chat.tsx` (229), `Skills.tsx` (191),
`JobOutput.tsx` (135), `Message.tsx` (128), `SubagentRail.tsx` (119), `Theme.tsx` (63).

---

## 1. Purpose & invariants

The subsystem guarantees: one composition root wires a store snapshot + keymap + components;
every component is a pure render of props; exactly one non-chat surface (the tabbed panel);
live work is always pinned on screen; destructive verbs are two-keypress (arm → confirm) with
the blast radius printed in between; every list's legend is its last row and names only bound
keys.

Invariant comments, verbatim (each file opens with one — these are the porting contract):

- **App.tsx**: "THE INVARIANT THIS HOLDS: **this file contains no logic worth testing.** Every
  decision it appears to make is made somewhere else and imported — what a key means
  (`keys.ts`), what the transcript looks like (`lines.ts`), what the state is (`store.ts`),
  what each surface renders …" / "SECOND INVARIANT — **no I/O of its own.** … the store owns
  the socket and this component reads it" (the concession: `AppControls` injected thunks) /
  "THIRD — **the mode is derived, not stacked.** `keys.ts` resolves a keypress against exactly
  one binding set … There is exactly ONE non-chat surface, the panel (spec §15)" / "FOURTH —
  **the transcript is built once.** `buildLines` produces the `VLine[]` that `Chat` paints,
  and it is memoized here … Two derivations would be two answers to 'which row is that'."
- **Chat.tsx**: "**presentational only.** Every value shown is a prop." / "SECOND — **the
  transcript hangs from the bottom.** A short conversation is padded above, not below" /
  "THIRD — **cost and context are chrome, not a panel**."
- **Message.tsx**: "**presentational only — props in, nothing out.**" / "SECOND — **a row is
  one terminal row.** `MessageRow` truncates to the exact display width with `truncateAnsi`
  … Measuring with `String.length` would let a styled line reflow."
- **Composer.tsx**: "**the cursor is exactly where the box says it is.** The text is wrapped
  here, into fixed-width chunks, rather than by the renderer" / "SECOND — **the box never
  grows past its cap.**" / "THIRD … **this component is presentational.**" / "`@` file
  candidates are expected to be gitignore-filtered at the source."
- **JobOutput.tsx**: "**presentational only.** The buffer, the row and the scroll offset are
  props; the fetch and the poll belong to the composition root."
- **SubagentRail.tsx**: "**the rail pins LIVE work only.** … a finished branch belongs in the
  tree … IT NOW HOLDS FOUR KINDS (spec §5: nothing runs invisibly) … SCHEDULES ARE THE
  DELIBERATE EXCEPTION to 'live only' … Enabled ones sit at the BOTTOM, count down instead of
  up, and `x` disables rather than kills."
- **Panel.tsx**: "**there is exactly one place that is not the chat** (spec §15) … one
  `PanelState`, one reducer, and a tab that is either showing or not." / "SECOND — **leaving
  the theme tab reverts an uncommitted preview** … wired *inside* `reducePanel` … `cancel()`
  is idempotent" / "THIRD — **the keymap is data, and it is not this file's data.** `TABS`
  lives in `tui/keys.ts`."
- **PanelHost.tsx**: "**`App.tsx` does not grow when a tab is added.**" / "SECOND — **one
  cursor, reset on every tab change.**" / "THIRD — **MCP state is re-fetched on every entry
  and never cached**" / "FOURTH — **absent capability is stated, never faked; and a CLOSED
  gap gets wired, not re-apologised for.**" / "FIFTH — **a row's budget is the host's
  business, and it is exact.**" / "NO I/O OF ITS OWN. Every fetch is an injected thunk."
- **Tree.tsx**: "**visibility is derived from lineage, never stored.** Spec §4 … there is no
  archive, deprecate, hide, or purge action."
- **Changes.tsx**: "**'not a repository' is an answer, never an empty diff.**" / "SECOND:
  **revert is the only mutation, and it is per path.** … it takes TWO keypresses: `x` arms …
  ⏎ performs."
- **ModelPicker.tsx**: "**the picker chooses the frontier model AND the cheap model.**" /
  "SECOND — **switching pins THIS session and moves the default for new sessions, and touches
  no other existing session.**" / "THIRD — **an id is a provider routing decision, so the
  catalog is injected.**"
- **Mcp.tsx**: "**the three states are never conflated.** A server can be *registered*,
  *granted* … and *connected* — and they are independent." / "SECOND — **nothing here is
  cached.**"
- **Skills.tsx**: "**an empty list, an absent source and a BROKEN skill are three different
  screens.**"
- **Theme.tsx**: "**the preview is the product, not a swatch.** … moving the cursor repaints
  the whole TUI … the revert is … in the panel's reducer."
- **Workflows.tsx**: "**a run that replayed nothing never looks like a run that worked.**
  (spec §8) … `replayRows` is therefore unconditional."

---

## 2. Public API

### App.tsx
- `App(props: AppProps)` — the composition root component; owns UI mode, draft, scroll,
  folds, selection, rail cursor, job scroll, attachments, history, and the key/mouse/paste
  dispatch. Renders one of four frames: help overlay, panel frame, job frame, chat frame.
- `type AppControls = PanelControls` — injected REST thunks (see PanelHost).
- `interface InputHooks { onPaste?, onMouse?, onNavKey? (register handler → unsubscribe fn);
  pasteClipboard?: () => Promise<{image: Blob} | {text: string} | null>;
  imageFromPasteText?: (text) => Promise<Blob | null> }` — streams taken off stdin by
  `mouse.ts` before the renderer sees them.
- `interface AppProps { store; defaultWorkspace?; home?; controls?; input?; models?;
  theme?; notifyDesktop?; copyText?; openUrl?; uploadImage?; now? }` — all transport is
  injected; `now` injected for reproducible renders/double-esc tests.
- `askPromptLines(prompt: string, rows: number, width = 80): string[]` — the ask card's
  prompt split on `\n` AND wrapped to `width - 4`; capped at `rows/3` lines with a
  `… N more lines` tail row.
- Internal but load-bearing: `inkKey(event) → {input, key}` (terminal-event → keymap shape),
  `Help` (the `?` overlay), `clampHelpOffset`, `StatusLine`, `AskCard`, `SelectionLayer`.
- Constants: `DOUBLE_ESC_MS = 600`, `SHELL_SESSION_TITLE = "shell"`, `WHEEL_ROWS = 3`,
  `CHAT_TOP = 2` (1-based row transcript starts on), `PANEL_TABS_ROW = 3`,
  `RAIL_POLL_MS = 1500`, `RAIL_TICK_MS = 1000`, `SCHEDULE_TICK_MS = 30_000`,
  `SECTION_MIN_TURNS = 8`, `HELP_STEP = 3`, `JOB_STEP = 3`, `JOB_POLL_MS = 1000`,
  `BRANCH_POLL_MS = 10_000`, `DOUBLE_ESC_SEQ = "\x1b\x1b"`.

### Chat.tsx
- `interface ChatMeter { model?, effort?, costUsd?, contextTokens?, contextLimit?,
  workspace?, branch?, shells?, agents?, runs?, help?, out?, note?, noteUrgent? }` — the
  status-line facts; worded by `format.ts::meterLine`.
- `interface ChatProps { lines: VLine[], width, height, scrollOff?, meter?, activity?,
  busy?, elapsedMs?, turnTokens?, tick?, queued?, notice?, decorate?, placeholder? }`.
- `Chat(props)` — virtualized window over pre-wrapped `VLine[]`; default placeholder
  `"type to start · the agent writes one program per round"`.

### Message.tsx
- `padRow(text, w): string` — pads to exactly `w` display columns AND flattens `\r?\n → " "`
  (this is what *clears* a row; a short row otherwise leaves the previous row's tail).
- `styledRow(text): StyledText` — ANSI escapes → styled chunks via `format.ts::ansiSpans`
  (bold/dim/italic/underline/reverse/strike, fg/bg hex, OSC-8 link). Chunkless input yields
  a single-space chunk (empty renders nothing = dropped row).
- `MessageRow({line: VLine, width, decorate?})` — one terminal row: `decorate` then
  `truncateAnsi` to width then `padRow`; empty text drawn as `" "`.
- `MessageView({message, width, isExpanded?, isFull?, streaming?, toolLogs?})` — whole
  message standalone via `lines.ts::messageLines`.

### Composer.tsx
- `interface ComposerProps { input, cursor, busy, width, maxRows, ghost?, attachments?,
  attachmentSel?, trigger?, completions?, completionSel?, completionMore?, keyboardOwner? }`.
- `composerHeight({input, ghost?, busy, width, maxRows, attachments}): number` — rows the box
  will draw: `2 (border) + shown text rows + clip-counter row? + hint row? + attachments.len`.
  Inner width = `max(4, width-4)`; text = `"› " + input + ghost + (ghost ? "  ⇥ tab" : "")`;
  cap = `max(2, maxRows)`; when clipped, one shown row is spent on the `…` counter. Hint row
  present iff `(busy && input !== "") || input.startsWith("!")`. **Must mirror the render
  exactly** — the container sizes the transcript from it.
- `completionPopupHeight(items, more): number = 2 + max(1, items) + (more>0 ? 1:0) + 1`.
- `Composer(props)` — the input box; `CompletionPopup({kind, items, sel, more})` — the `@`/`/`
  menu; `PopupLabel` — fuzzy-highlight label (bold+accent on matched indices, dim dir prefix).

### JobOutput.tsx
- `interface JobOutputProps { id, job: BackgroundJob|null, output: string, scroll?, width,
  height, now, error?, armed? }`.
- `JobOutput(props)` — header (`⚙ name  status`), command sub-lines, padded body window,
  blank row, footer.
- `jobSubLines(job, id, width, height): string[]` — `"{id} · pid {pid} ·" + oneLine(command)`
  wrapped to width, capped at `max(1, (height-3)/2)` with ellipsis on the last kept row.
- `jobBodyRows(height, subRows = 1): number = max(1, height - 3 - subRows)` — the page step.

### SubagentRail.tsx
- `liveSubagents(children: SessionRow[]): SessionRow[]` — `isDelegated(kind) && busy`,
  sorted by `createdAt` ascending (start order — the cursor must not jump).
- `railHint(units): string` — `"↓ 2 shells · 1 agent running · 1 scheduled"`; counts per
  kind; schedules counted apart ("running" would be a lie about a countdown).
- `SubagentRail({units: LiveUnit[], sel: number|null, width, armedId?})` — one row per unit
  via `format.ts::unitLine`; `sel === null` (composer focused) appends the hint row instead
  of a cursor. Empty units ⇒ renders nothing at all (`null`), not an empty box.
- Re-exports `DELEGATED_KINDS` from `forest.ts`.

### Panel.tsx
- `interface PanelState { open: boolean; tab: PanelTab }`; `initialPanel = {open:false,
  tab:"tree"}` (the tree is the home tab).
- `type PanelAction = toggle | close | jump{tab} | cycle{delta} | move{delta} | confirm |
  confirmSummarize`.
- `panelActionFor(command: Command): PanelAction | null` — the whole seam keys.ts → panel.
  Maps `tab.*` chords via `tabForCommand`, plus `panel.toggle/close/next/prev/confirm/
  confirmSummarize` and `move.up/down`.
- `reducePanel(state, action, deps {theme?: {move, commit, cancel}}): PanelState` — pure but
  for the theme preview: EVERY departure from the `theme` tab (close, toggle, jump elsewhere,
  cycle) calls `theme.cancel()`; `move` on theme calls `theme.move(delta)`; `confirm` on
  theme calls `theme.commit()`. `jump` to the already-open tab closes the panel.
- `tabAtColumn(active: PanelTab, col): PanelTab | null` — click hit-test on the strip;
  inactive tab renders `"  "+title`, active `" ["+title+"]"`; only the title is a target,
  padding hits nothing; must walk the exact widths `PanelTabs` paints.
- `PanelTabs({tab, width?})` — the strip; below fitting width collapses to
  `[tab] n/N · ⇥ next · ^t close`.
- `panelBodyRows(rows): number = max(0, rows - 1 - gapRows(rows))`; `gapRows = rows>=5 ? 1:0`.
  Floor is ZERO by design (a floor is a claim about space; false claims caused row
  corruption — see §4).
- `Panel(props: {tab, rows, width?, changes?, model?, mcp?, skills?, theme?, children?})` —
  outer box `height = rows+2` (border), tab strip, then body inside a fixed-height
  clipping box; zero-row body is not mounted at all. Re-exports the tab bodies + `TABS`,
  `PANEL_TABS`, `tabForChord`.

### PanelHost.tsx
- `nameFromUrl(raw, taken = []): string` — registry name from server URL: hostname minus
  `mcp.`/`www.`/`api.` parts, minus TLD (and the 2nd-level part of a `co.uk`-shaped ≤3-char
  suffix), slugged `[a-z0-9-]`; collisions get `-2`, `-3` …; non-URL → `""`.
- `interface PanelControls { forkAt?, extractFrom?, moveIntoOpen?, listChildren?, loadMcp?,
  setMcpEnabled?, beginMcpAuth?, clearMcpAuth?, deleteMcpServer?, connectMcpServer?,
  restartMcpServer?, mcpAuthStatus?, putMcpServer?, loadSkills?, pauseWorkflow?,
  resumeWorkflow?, stopWorkflow?, rerunWorkflow? }` — every one a REST call, injected.
- `interface PanelHostDeps { store, state, rows, cols, now, controls?, models?,
  theme?: {current?: ThemeState|null, persist?}, forest (ForestInput minus
  currentId/filter/userOnly), expand, collapseTurns, drillIn, collapse }`.
- `interface PanelHandle { open, tab, handle(command, input?) → bool ("was it mine"),
  scrollBy(rows) → bool (only a focused diff consumes a wheel tick), filtering,
  filterInput(text), openRun(runId), view }`.
- `usePanelHost(deps): PanelHandle` — the controller (all tab state, fetch-on-entry,
  confirm dispatch). Constants: `AUTH_POLL_MS = 2000`, `AUTH_POLLS = 150` (5-min OAuth
  poll), `SKILLS_NOTE`, `MODEL_NOTE = "pinned for this conversation and set as the default
  for new ones"`, `NO_SESSION_CHANGES` (available:false, reason "no conversation is open…").

### Tree.tsx
- `kindGlyph(s: SessionRow): string` — `root:"●", fork:"⑂", compaction:"≣", subagent:"◆",
  workflow_agent:"◈" (nothing creates it; kept for schema totality), schedule_run:"◷",
  shell:"●"`; a `root` **with** `originId` gets `"↦"` (derived root: handoff/extract).
- `statusMark(s, busyBelow = 0): {glyph, color} | null` — busy or busyBelow>0 → `⋯` cyan;
  `outcomeOk === false` → `✗` red (checked BEFORE turn status: delegation outcome);
  lastTurnStatus running→`⋯`cyan, orphaned/interrupted→`◼`yellow, error→`✗`red,
  done→`✓`green; never-ran → `null` (absence, not a state).
- `titleOf(s): string` — strips `/^(fork|compacted|handoff|subagent|workflow) · /`, falls back
  to last workspace path segment, then `"(untitled)"`.
- `forestWindow(count, selected, rows, chrome = 0): {start, height}` — reserves exactly 2
  fixed legend rows (mark legend + key legend) + chrome; uses `format.ts::windowAround`.
- `markLegend(rows: ForestRow[]): string[]` — explains only glyphs present on screen; kinds
  first (`● yours, ↦ handoff, ⑂ fork, ≣ compaction, ◆ subagent, ◷ scheduled run`) then
  statuses (`⋯ running, ✓ done, ✗ failed, ◼ stopped`).
- `Tree({rows, selected, height, filter?, filtering?, workspace?, cols?, message?})`.
- `interface TreeProps` — as above; `workspace` = open conversation's workspace so a
  top-level row in a DIFFERENT one shows its dir (only when it differs).

### Changes.tsx
- `interface ChangeItem { file: FileDiff; added: number; removed: number }`.
- `fileStats(f): {added, removed}` — counts `+`/`-` line prefixes over hunks.
- `changeItems(set: SessionChangeSet|null): ChangeItem[]` — empty unless `set.available`.
- `diffBody(f?): string[]` — hunks flattened `[header, ...lines]`, control bytes → `·`
  (tab kept); no hunks → `"(binary file — {status}, contents not shown)"` if `f.binary`
  else `"(no textual diff — {status})"`.
- `type PendingRevert = {scope:"file", item} | {scope:"all"}`.
- `revertScope(item, total): string` — added→"reverting DELETES it", deleted→"reverting
  RESTORES it", modified→"reverting DISCARDS +a -r"; appends "; the other N file(s) are
  untouched".
- `NOT_A_REPO_HINT` — the spec-§13 non-git sentence.
- `Changes(props: ChangesProps {cols?, set, items, selected, scroll?, rows, focused?,
  message?, pending?, hint?})` + private `RevertConfirm`.

### ModelPicker.tsx
- `type EffortChoice = "default" | Effort`; `EFFORTS = [default, low, medium, high, xhigh,
  max]`; `asEffortChoice(value): EffortChoice | null` (unrecognised → null, never a fake row).
- `interface ModelConfig { defaultModel, sessionModel: string|null, cheapModel: string|null,
  defaultEffort, sessionEffort: EffortChoice|null }`.
- `effectiveModel(cfg)`, `effectiveEffort(cfg)` — session pin ?? default.
- `type Tier = "frontier"|"cheap"|"effort"`; `ModelEntry` discriminated on tier (effort ids
  are `EffortChoice`, model ids free strings); `SECTIONS` titles/hints (hints < 70 chars on
  purpose — 80-col floor).
- `interface ModelFilters {frontier: string; cheap: string}`; `NO_FILTERS`; `type ModelTier`.
- `modelEntries(catalog, {cheapCatalog?, filters?, score?}): ModelEntry[]` — frontier rows,
  cheap rows, then 6 effort rows; per-tier query RANKS (score desc, ties keep catalog order).
- `isActive(cfg, e)`, `chooseEntry(cfg, e): ModelConfig` — frontier pick writes sessionModel
  AND defaultModel; cheap writes cheapModel only; effort writes sessionEffort AND
  defaultEffort. Pure; caller does the write.
- `CHEAP_UNSET`, `NO_MATCH` — sentences; `type DisplayRow = {header} | {hint} | {search} |
  {entry, index} | {note}`.
- `displayRows(entries, {cheapUnset?, filters?, focused?}): DisplayRow[]` — built section by
  section so an empty-matching section still shows its header + search box.
- `modelWindow(display, selected, rows, chrome=0): {start, end, height, marks}` — reserves
  1 legend row; `marks` = one combined `↑ n · ↓ m more` row only when it fits (avail >= 3).
- `visibleEntries(display, start, end): number[]` — the entry indices digits `1-9` address
  (headers/hints/notes are NOT numbered).
- `ModelPicker(props {cols?, cfg, entries, selected, rows, message?, filters?, focused?})`.

### Mcp.tsx
- `interface McpTabProps { status: McpStatus|null, selected, message?, rows?, cols?,
  entry?: string|null (URL being typed) }`.
- `mcpWindow(count, selected, rows, chrome=0): {start, end, height, counter}` — reserves 2
  legend rows (mark legend + key legend).
- `hasStaticAuth(entry?): boolean` — any non-empty `Authorization` header on the registry
  entry.
- `mcpDetail(status, name): string` — dim tail `granted|off · N tools · error · authed|
  keychain|needs auth · url-or-command` (keychain = static auth header OR
  `isCoveredHost(url)`).
- `statusLegend(names, status): string[]` — present-only: `● connected`, `◐ granted, not
  connected — c connects`, `○ not granted — ⏎ grants`.
- `McpTab(props)`.

### Skills.tsx
- `interface SkillSourceRow {source, dir}`; re-exports `SkillRow` (type-only import from
  `server/skills.ts`).
- `skillsWindow(count, selected, rows, chrome=0): {start, height, counter}` — 1 legend row.
- `SkillsTab({skills: SkillRow[]|null, rows, cols?, selected?, note?, sources?, filter?,
  filtering?})` — null skills → `note` (warn) or `loading…`; broken skill rendered in error
  color with its `error` text in place of the description.

### Theme.tsx
- `ThemeTab({preview: ThemePreview|null, rows})` — preset rows with per-preset swatch
  (`presetSwatch`, resolved from the preset's own colors, never the live palette); legend
  `"{previewing|current:} {name} — ↑↓ preview live · ⏎ keep · esc back (leaving reverts)"`.

### Workflows.tsx
- `type Tone = "text"|"muted"|"accent"|"warn"|"error"|"info"`; `Cell {text, tone?, bold?}`;
  `type Row = Cell[]`; `rowText`, `linesOf` — the testable rendering unit (pure rows).
- `tokenChip(n): string` — `"" | "{fmtTokens} tok"`.
- `windowed<T>(items, sel, rows): {slice, from}`.
- `wfGlyph(status): {glyph, tone}` — queued `◦`muted, running `◐`info, paused `⏸`warn,
  done `✓`accent, cached `≡`accent, error `✗`error, stopped `■`warn, default(orphaned)
  `⚠`warn.
- `runGlyph(status, failed)` — `done` with `failed>0` → `⚠`warn (no lying checkmark).
- `phaseGroups(run, agents): PhaseGroup[]` — declared phases (in script order, INCLUDING
  ones with no agents), then undeclared phases agents reported, then phase-less (`""`);
  groups dropped only if both empty and undeclared.
- `WF_FILTERS = [null, "running", "queued", "done", "error"]`; `visibleAgents(agents,
  filter)` — "done" folds in "cached".
- `replayRows(replay: ReplaySummary): Row[]` — UNCONDITIONAL; counts row
  `"N replayed · N ran live [· N still going] · of N [· N available to replay]"`; the
  server's canonical `line` as a second row only when `sourceId || alarm`; alarm =
  `available > 0 && replayed === 0` (error tone, bold).
- `costRows(cost: RunCost): Row[]` — labelled `≡ usage` (tokens + agent count; no dollars —
  RunCost has none); per-phase breakdown only when >1 group.
- `warningRows(warning: LargeRunFlag|null)` — `! large` + reasons +
  "advisory — nothing is throttled; x stops the run".
- `steerActions(status, live): SteerAction[]` — running+live: `p pause (finishes in-flight
  agents), x stop`; running+!live: `x stop — orphaned by a restart, e script`; paused:
  `p resume, x stop, e script`; settled: `r rerun (replays the journal), e the script + path
  to edit (then r), s save to run again by name`.
- `runHeaderRows(detail, {lastLog?, now?}): Row[]` — glyph+name+status+`relaunch of X`+
  `(not live here)` when running but not held by this process; description + settled/total
  agents + failed + elapsed; then replayRows, costRows, warningRows, error row, live-log row.
- `phaseRows(groups, selected, cursor, current)` — ordinal until complete, then ✓/✗;
  `done/total` per phase; current phase title bold.
- `agentRows(agents, selected, cursor, compact, now)` — glyph + label (clip 16/34); full
  form adds `model · Nk tok` and clock (queued shows "queued", not a clock).
- `agentDetailRows(agent, promptOpen, now)` — status line, `session {id8} — o opens it` /
  "no session — this call was replayed from the journal", collapsed Prompt (2 lines +
  `… N more lines`; ⏎ expands), Activity, Outcome/Error (error leads for failed agents;
  cached says "replayed from the source run's journal — no agent ran").
- `scriptRows(detail)` — mirror path FIRST (`detail.scriptFile`), live-run warning or
  `"r relaunches a NEW run from this one's journal · N calls journaled here"`, then
  line-numbered script.
- `type WfLevel = 0|1|2|3|4` (runs · phases · a phase's agents · one agent · script).
- `BOUND_STEER_KEYS = {"p","P","x","r","e","s"}` — footer may only name bound keys.
- `footer(level, detail): string` — per-level; always ends `esc back`.
- `wfRunsHeight(rows) = max(0, rows - 1 - 2*wfGap(rows))`; `wfGap(rows) = rows>=8 ? 1 : 0`.
- `Workflows(props: WorkflowsProps {runs, sel, level, detail, phaseSel, agentSel, scroll,
  filter, promptOpen, rows, cols, lastLog?, now?})` — level 0: run list + footer; level 4:
  header + script window + footer; levels 1–3: header + Miller columns (left pane width
  `min(24, max(12, cols/4))`, right pane grows) + footer. Header is CLIPPED to fit rather
  than pushing panes off.
- `WorkflowChip({run, log?, now?})` — the composer-side live-run line (also reports the
  replayed count).

---

## 3. Data structures

Wire/state shapes the components consume (exact field names; owned by other modules):

- `VLine` (lines.ts): `{ text: string; click?: string; copy?: string; src?: … }`. `click`
  grammar: a fold key toggles; `"<key>!full"` lifts a block's line cap; `"open:<sessionId>"`
  descends into a branch; `"job:<sessionId>:<jobId>"` opens job output;
  `"workflow:<runId>"` opens the run view.
- `SessionRow` (api.ts): `Session & { busy: boolean; lastTurnStatus?: TurnStatus;
  costUsd?: number; tokens?: number }`. Session carries `id, title, kind (root|fork|
  compaction|subagent|workflow_agent|schedule_run|shell), workspace, originId, createdAt,
  model, effort, contextTokens, draft, outcomeOk …`.
- `LiveUnit` (store.ts): `{ kind: "shell"|"subagent"|"workflow"|"schedule"; id; sessionId;
  title; elapsedMs (negative countdown for schedules); tokens: number|null;
  costUsd: number|null; progress; detail }`.
- `ForestRow` (forest.ts): union of `{kind:"session", id, session, depth, open, delegated,
  current, busyBelow, expandable}` | `{kind:"message", id, sessionId, role, gist, depth,
  last, active, matched}` | `{kind:"section", id, sessionId, label, depth}` |
  `{kind:"collapsed", id, originId, count, depth}`.
- `ForestInput` (forest.ts): `{ sessions, childrenByOrigin, threads, expanded, drilled,
  sections?, currentId?, filter?, matchedSessions?, matchedMessages? }` — "absent thread ≠
  empty thread" is load-bearing.
- `SessionChangeSet` (server/changes.ts): `{ available: boolean; reason?: string;
  base: string|null; files: FileDiff[]; workspace }`. `FileDiff`: `{ path, status:
  "added"|"modified"|"deleted", binary?, hunks: {header, lines: string[]}[] }`.
- `McpStatus` (mcp/status.ts): `{ registry: { servers: Record<name, {url?, command?,
  headers?}> }; active: string[]; connections: {server, alive, toolCount, error?}[];
  auth: Record<name, {authorized: boolean}> }`.
- `SkillRow` (server/skills.ts): `{ name, description?, error?, mcp?: string[] }`.
- `BackgroundJob` (schema/parts.ts): `{ name, command, pid, status ("running"|…),
  startedAt, exitedAt, exitCode: number|null, signal: string|null }`.
- `WorkflowSummary`/`WorkflowDetail` (api.ts): summary `{id, name, description, status,
  createdAt, finishedAt, currentPhase, resumeOf, agents: {done, total, cached, failed}}`;
  detail `{workflow: WorkflowRun (incl. phases: {title, detail}[], script, error,
  currentPhase), agents: WorkflowAgentView[] ({label, status, phase?, model, tokens,
  toolCalls, startedAt, finishedAt, prompt, activity: string[], error?, result?,
  sessionId?}), replay: ReplaySummary {replayed, ranLive, pending, total, available,
  sourceId, line}, cost: RunCost {tokens, agents, byPhase: {phase, tokens}[]},
  warning: LargeRunFlag|null {reasons: string[]}, live: boolean, scriptFile: string}`.
- `ModelRow` (llm/client.ts): `{ id, label, provider }`.
- `ThemePreview` (theme.ts): `{ presets, index, name, previewing, move(delta), commit(),
  cancel() }`; `presetSwatch(p) → {token, color, block}[]`.
- REST calls made directly by App/PanelHost via `api.ts` (the rest are injected thunks):
  `api.listSkills()`, `api.ghostText(id) → {ghost: string|null}` (`POST /sessions/:id/ghost`),
  `api.sections(id, {turns: {gist}[]}) → {sections: {start, end, label}[]}`,
  `api.putDraft(id, text|null)` (`PUT /sessions/:id/draft`), `api.getModelSettings() →
  {defaultModel, cheapModel, defaultEffort}`, `api.listFiles(id)` / `api.listFilesIn(dir)` →
  `{files}` (git ls-files, gitignore-filtered), `api.listDirEntries(prefix, ws?) →
  {entries}` (for `@~/…`, `@/…` browsing), `api.branch(dir) → {branch}`, `api.fork(id,
  {atMessageId, exclusive?, summarizeAbandoned?})`, `api.extract(id, {picks})`,
  `api.moveInto(id, {sourceId, picks})`, `api.getSession(id)` (thread for expanded rows),
  `api.getWorkflow(id)`, `api.saveWorkflowAs(id, name)`, `api.revertChanges(id, paths?)` →
  `{reverted: string[], skipped: string[], failed: {path, error}[]}` (**`paths: undefined`
  means the whole set; an empty array is refused server-side**).
- No DB tables are touched by this subsystem; everything goes over the loopback HTTP API.

---

## 4. Behaviors & edge cases

### Frame layout (App)
Three fixed regions + one growing one, top to bottom: header (row 1, one line: session
title + `· disconnected` warn suffix), the growing region (transcript | panel | job view),
then pinned: SubagentRail (`railH = units.len == 0 ? 0 : units.len + (mode=="rail" ? 0:1)`
— the +1 is the hint row), the composer (or AskCard), the status line (1 row). Every fixed
region reports its OWN height (`composerHeight`, `completionPopupHeight`, ask card =
`4 + promptLines + options.len`), and `chatH = max(1, rows - 1 - railH - inputH - 1)`. The
same `chatH` is used by the renderer AND the mouse hit-test — two copies puts a click one
row off. Terminal size: `cols = max(20, width || 80)`, `rows = max(8, height || 24)` —
`||`, not `??`: a tty-less renderer reports 0 and a zero width wraps one char per line.
Panel frame: panel body height also subtracts a replay row and a notice row when present;
`panelRows = panelBodyH + 2` (Panel draws rows-2 of chrome twice on the way down).

### UI modes
`UiMode = chat | ask | rail | job | help | panel`, but stored `mode` only holds
chat/rail/job/help; the effective mode is derived per render:
`uiMode = panel.open ? "panel" : (mode == "chat" && ask ? "ask" : mode)`. A held `ask()`
replaces the composer and owns the keyboard; the panel outranks both. **No mode may strand:**
if `units` empties while `mode == "rail"` → back to chat and `railSel = 0` (else clamp
railSel); if `mode == "job"` and `state.jobView` is null → chat. Opening a job is
**fetch-first, then set mode** (`openJobView`) — the reverse order races the stranded-guard
and bounces back every first open.

### Keyboard dispatch
Every keypress: adapt terminal event → `(input, key flags)` (`inkKey`): modified keys take
`name`, unmodified printable single chars take the raw sequence (preserves capital letters);
macOS Option maps to meta; `escape` clears the meta flag (ESC ESC arrives as ONE event
flagged meta). Then `chord = chordOf(input, key)`; any chord other than `ctrl+c` disarms
`quitArmed`. Build a `KeyContext` **reading the draft and quitArmed from refs, not render
state** — a fast-typist burst is processed before any re-render and stale reads ate typed
`?` characters and broke `^c^c`. Context fields: `mode, tab (panel's open tab — the scope
bare letters resolve in), panelFiltering, emptyDraft, inSubagent (originId present),
multiline, busy, doubleEsc, quitArmed, justSent (lastSendAt within UNSEND_MS, read at the
keystroke — it expires on the clock), railLive, completing, hasAttachments`. Then
`lookup(ctx, chord)` → `Command` → `run(command, rawInput)`; unresolved printable text
inserts into: askText (ask mode), panel filter buffer (panel mode + filtering), or the
draft (chat mode only; also clears completion `dismissed`). All inserted text goes through
`stripCtl`.

**Escape** is special-cased ahead of lookup: if a hold timer is pending, a second esc within
the window cancels it and runs `draft.clear` (double-tap). `doubleEsc = sequence ==
"\x1b\x1b" || now - lastEsc < 600ms`. The AMBIGUOUS case only — resolved command is
`turn.interrupt`, not doubleEsc, and the draft is non-empty — is HELD for 600ms then fires
`turn.interrupt`; every unambiguous case fires immediately (a stop at an empty composer is
never delayed). Test contract: "esc esc still stops a running turn — the rewind never
shadows the stop".

### `run(command, input)` — the command switch
Panel gets first refusal: `if panel.handle(command, input) return`. Then (chat-side arms):
- `image.paste` (^v-as-command): `pasteClipboard()`; text ≤50 chars inserts, >50 becomes a
  held paste; image uploads → attachment. Missing hooks → notice.
- `quit.arm` → notice "^c again to quit — subagents and workflows keep running";
  `quit` → **save the draft first** (`PUT draft` raced against a 300ms timer — a dead server
  must not make ^c^c hang), then destroy the renderer via microtask.
- `help.open`/`help.close` — both reset `scrollOff` to 0 (help and transcript share it).
- `send`/`send.queue` → `submit(queue)` (below).
- `session.new` — clears draft, histAt, scrollOff, attachments, pastes; `store.newConversation()`.
- `schedules.show` / `saved.show` / `artifacts.show` / `rules.show` — store describe-calls
  (notices/records).
- `session.copyId` — id read from `store.getState()` (NOT the captured render state);
  `copyText?.(id)`; notice `copied {id}` or just the id when no clipboard.
- `session.compact` — `store.compact(input)`; the distilled prompt lands IN THE COMPOSER.
- `draft.clear` — clears draft + histAt + attachments + pastes.
- `cancel` — reset scroll, dismiss notice. `turn.interrupt` — no busy check here; the
  keymap only routes it while busy.
- `message.unsend` — the take-back within `UNSEND_MS` of a send. `takeBackTarget(queued,
  thread)` (forest.ts) decides: `queued` → pop back into composer locally; `none` → do
  nothing (never fall through to a stop); sent → `store.unsend(atMessageId)` — ONE server
  call that stops the turn and deletes the message + partial answer; returned text goes to
  the composer; notice differs by busy.
- `attachment.up/down` — move between editing-text (null) and queued image rows.
- `history.prev/next` — `sent[]` ring; ↑ from null starts at the last; ↓ past the end
  clears. **Any route back to an empty draft resets `histAt`** (an effect watches
  `line.text == ""`).
- `ghost.accept` (⇥ with no popup) — replaces the draft with the ghost, clears it.
- `complete.accept` — a popup row with `run` (built-in slash command) removes the trigger
  token from the draft and RUNS the command; otherwise `applyCompletion` inserts. `complete.
  prev/next` move; `complete.dismiss` sets `dismissed` (sticky until the trigger token
  changes; typing re-opens).
- `fold.all` (^e) — toggles `foldAll` and CLEARS `openKeys`/`fullKeys` (the global toggle
  wins; ^e twice is a reset).
- `session.out` (←) — open `session.originId`, and ONLY for a session that was drilled
  into (a collapsed kind: subagent / workflow agent / schedule run). A handoff, a fork and
  an extract set `originId` as lineage for the tree; each is a new conversation with no way
  back, and the `← back` chip, the guard and the destination all read the one predicate.
- `scroll.up/down/pageUp/pageDown` — three surfaces, different senses: transcript offset
  counts UP from the bottom (scroll up raises it, page = `max(1, rows-8)`, clamp to
  `lines.len-1`); help is a top-down document (`HELP_STEP=3`, clamped to
  `helpLines().len - (rows-2)`); job view counts up from the tail (`JOB_STEP=3` /
  `jobPage()` = `jobBodyRows` of the current geometry; no upper clamp — JobOutput clamps
  to its own buffer).
- Rail: `rail.enter` (↓ from empty composer) → mode rail, sel 0, disarm; `rail.up` at row 0
  exits to chat; `rail.down` clamps; **every cursor move disarms `armedStop`** (a
  confirmation must not outlive the row it was read on). `rail.open` (⏎): shell → open THAT
  job's output (jobScroll=0, fetch-first); schedule → `store.describeSchedules()` (cursor
  stays); else open the unit's session (mode chat). `rail.stop` (x): first press arms +
  notice naming the blast radius (shell: `x again to kill {title}[ — {detail}]` with detail
  dropped when it repeats the title; schedule: "…disable… will not fire until re-enabled";
  else "…work in flight is lost"); second press `store.stopUnit(u)`. `rail.exit` (esc) →
  chat.
- Job view: `job.close` (esc) → back to RAIL if units remain else chat; `job.stop` (x) —
  same arm/kill idiom on the watched job (running only).
- Ask: `ask.pick` (digit 1-9 → `ask.options[n-1]`), `ask.send` (typed text, non-empty),
  `ask.decline` (esc).
- `delete.back` — with an EMPTY draft and an image row selected, deletes that attachment
  (held pastes are removed by deleting their mark in the draft — ordinary editing);
  otherwise `editLine`.
- default → `editLine(state, command)` (pure line editing in keys.ts).

### submit(queue)
Reads the draft from the REF (React batching: a burst's Return sees pre-burst state
otherwise — "typed text and its Return in ONE read send that text"). Empty text with no
attachments → no-op. Then in order:
1. **`!command`** → the user's own shell, never a message: clear draft, push the verbatim
   `!`-prefixed text into `sent` history (↑⏎ re-runs), and `store.runShell(command)`. With
   no open session: find-or-create ONE per-workspace conversation of `kind: "shell"` titled
   `"shell"` (never per-command; test: "reuses one shell conversation instead of minting
   one"). Nothing billed, nothing in the thread; the job lands on the rail.
2. **Unknown `/word`** → `unknownCommand(text, skillNames)`: notice `there is no /X — did
   you mean /Y? · type / for the list, or ? for every key`; **draft kept** for editing.
   (Pasted `/model` must run, not reach the model — test pair: "a PASTED /command runs";
   "a message that merely BEGINS with a command is still a message".)
3. **`slashInvocation`** → clear draft, dispatch the command (queueing ignored — opening a
   panel never waits for a turn).
4. Otherwise: capture attachments + pastes, clear all composer state, append to `sent`,
   reset scroll, `expandPastes(text, pasted)` (marks expand in place; a deleted mark drops
   its paste), then `store.send(message, {queue, images})` — or create a session on
   `defaultWorkspace` first when none is open.

### Mouse
Registered once via `hooks.onMouse`. Wheel: panel gets first refusal (`panel.scrollBy(±3)`
— only a focused diff consumes; otherwise transcript offset ± `WHEEL_ROWS`). Drag protocol
(all through `selRef`, since a burst batches): `down` → snapshot the painted screen
(`screenRows()` decodes the renderer's real char buffer ONCE — re-reading mid-drag would
read the highlight back) and open `{anchor, focus}` at the point; `drag` → move focus;
`up` with a non-empty selection → **copy on release** via `selectedCopy`, preferring the
transcript's own styled `VLine` per row (`rowAt`, which shares `chatBodyHeight`/
`lineAtSlot` with the renderer) and falling back to the painted snapshot for panel/rail/
composer rows; notice `copied N characters`; **selection dropped immediately** (the notice
shifts rows; a kept screen-coordinate highlight would slide). `up` on an empty selection =
a click: try links first — `linkAt` on the transcript row's OSC-8 markers at the exact
column, else `urlAcross` over the plain painted rows (rejoins wrapped URLs; this is how
the mcp tab's auth link is clickable); a click NEXT TO a link opens nothing. Then
`clickAt(y, x)`:
- Panel open: only row `PANEL_TABS_ROW` (3) is live — `tabAtColumn(panel.tab, x-2)` →
  `tab.<id>` command via `runRef`.
- Transcript rows (`CHAT_TOP ≤ y < CHAT_TOP+chatH`): resolve `lineAtSlot(...).click` —
  `open:` → `store.open`; `job:` → open job output; `workflow:` → `panel.openRun`;
  `*!full` → add to `fullKeys` (sticky; re-capping is ^e); else toggle in `openKeys`.
- Rail rows: select the unit and enter rail mode (the row's own legend becomes true).

Selection highlight: an absolutely-positioned overlay layer per selected row, EXPLICIT
`fg=palette.bg, bg=palette.accent` — **never terminal INVERSE**, which double-flips to
white-on-white on OpenTUI (same defect for the caret and the list cursors). Row spans are
sliced from the painted snapshot with ANSI-aware slicing; empty spans (drag past EOL) are
skipped. NavKeys (home/end/shift-tab/forward-delete come via `onNavKey`; the stdin filter
eats CSI Z etc.): shiftTab → `panel.prev`; forwardDelete → `delete.forward`; home/end →
cursor.home/end.

### Clocks & polling (App owns every clock)
One interval, three rates: `SPINNER_MS` while a turn is busy; `RAIL_TICK_MS` (1s) while
anything is live (rail branches, running jobs, running/paused workflows); `SCHEDULE_TICK_MS`
(30s) when only enabled schedules exist; no timer otherwise. The turn clock is
`state.turn` (startedAt/endedAt/tokens) — never a local ref. Children (`GET /sessions?
originId=`) are polled every 1.5s while busy OR while the rail still shows a live child (a
subagent outlives its spawning turn; one-shot pulls left a ghost `◆ running` row); a failed
poll is a stale rail, never an error. The open job re-fetches every 1s while running; the
fetch that sees an exit is the last. The branch (`git rev-parse` server-side) polls every
10s. Ghost text: only on an idle session, empty draft, non-empty thread; debounced 400ms;
every failure is silence. Sections: fetched once per expanded conversation with ≥8 turns
(gists = first 200 chars of each turn's text parts); marked in-flight before the fetch so a
re-render doesn't double-ask. Files for `@`: fetched once per session (or workspace when no
session), thousands of paths ranked locally. Skills + default model: once per process.
Background-finish (`state.background.seq`): each seq bump raises notice
`✓ {title} finished — ^t opens the tree` (**^t not ^s**: ^s is guarded on an empty draft
and a fresh fork prefills the composer) + `notifyDesktop`.

### Draft persistence
`PUT /sessions/:id/draft` on the way OUT of a session (effect cleanup keyed by session id,
drafts held in a per-id map — a single `{id, text}` ref reads the NEW session's empty
composer by cleanup time and stashes to the wrong session), and on quit (raced, see above).
Restored on the way IN only once the snapshot for THAT id has arrived
(`state.session?.id === id`), only into an empty composer, tracked by a `restoredFor` ref.

### Completion (`@` and `/`)
`activeTrigger(text, cursor)` (format.ts) finds the token; `dismissed` suppresses it.
`@~/…` and `@/…` are filesystem browsing: `browsePrefix` extracts the directory already
typed, ONE directory is fetched (`listDirEntries`), entries rank against the query; until
the fetch for THIS prefix lands the popup is empty (previous directory's entries must not
rank against a foreign query); picking a directory keeps the trailing `/` so the next fetch
drills down. Repo `@`: candidates = `files` (gitignore-filtered server-side). `/`:
`SLASH_COMMANDS` (with `run` command + desc) FIRST, then skills — source order breaks
score ties, so the built-in `/skills` outranks a skill named "skills". Cursor clamps to a
shrinking list. Ghost is suppressed while completing (`ghost: completing ? "" : ghost`).

### Chat rendering
`body = chatBodyHeight(height, queued.len, notice?)` — the scroll-indicator row and the
activity/busy strip are RESERVED unconditionally (appearing/vanishing rows made the
transcript jump a row at turn start/end); queued rows and notice row are counted. Slots are
keyed by SCREEN ROW, never transcript index (index keys remounted every node per streamed
token). Pad ABOVE (`pad = body - rows.len`); the empty-transcript placeholder occupies the
LAST slot (where the first reply lands). Scroll indicator: `↓ N more lines below · P%`
(percent = viewport top's position; fully scrolled up = 0%). Queued rows: `⧖ queued: {q}`
dim. Busy strip: `busyLine({activity, elapsedMs, tick, tokens})` — spinner glyph accent +
dim rest; not busy but activity present: `⋯ {activity}`; else a blank padded row. Notice:
warn color, padded. Placeholder overrides (App): fresh fork with empty thread →
`branched here · say it differently` (0 changed files) or `branched here · the files were
not rewound — N files still changed on disk · ^t ^d then X reverts them` (the key sequence
is load-bearing and was walked end-to-end; ^r is deliberately NOT bound); no session at all
→ `type to start · ^f reopens "{recent title clipped 40}" and everything before it` when a
non-collapsed recent session exists.

### Message row rendering (ratatui note)
The two halves of row hygiene are non-negotiable: (1) every emitted row is padded to full
viewport width (clears the previous frame's longer row) with embedded newlines flattened
(one string painting two rows shifts every pinned region below); (2) styled text is parsed
into spans/chunks — never raw SGR passed to the backend (in OpenTUI raw escapes desynced
the cell diff; in ratatui you'd never emit raw escapes anyway, so `ansiSpans` → `Span`s is
the natural port). A truncate must be ANSI- and width-aware (CJK wide glyphs count 2).

### Composer rendering
Wraps itself into fixed `innerW = max(4, width-4)` chunks (border + paddingX); cursor row
found by offset arithmetic (at a row end only if no row continues it); when clipped to
`cap = max(2, maxRows)`, windows `cap-1` rows centered on the cursor row plus a counter row
`… N lines above · M below`. First row carries the accent `"› "` prefix. Ghost text is dim
from `ghostStart = 2 + input.len` onward with a `"  ⇥ tab"` tail hint. Caret = explicit
`fg=palette.bg on bg=accent` block over the char under the cursor (space at EOL);
suppressed entirely when `keyboardOwner` is set (a block cursor is the strongest possible
focus claim). Placeholder logic: keyboardOwner + empty input → `"{owner} has the keyboard
· esc returns here"`; empty + no ghost → `"type a message · enter sends"`; a ghost
SUPPRESSES the placeholder (they'd collide in the same cells). Border color: muted when
keyboardOwner, warn when busy, accent otherwise; background `palette.panelInset`.
Attachment rows: `❯ [image: name]` (accent when selected) — images only, held pastes have
their mark in the draft. Hint row: busy+non-empty → "enter interjects this turn";
`!`-prefixed → "runs in your shell · not a message · output lands in the rail".
CompletionPopup sits ABOVE the box; an empty match still shows the box ("no matching
files" / "no matching commands or skills" — hiding it reads as broken); legend: file →
"files & dirs — ↑↓ select · ⏎ or ⇥ inserts · esc closes"; command → "… ⏎ runs or inserts
…". `↓ N more — keep typing to narrow` when capped.

### Ask card
Replaces the composer inside a warn-colored rounded border: prompt lines (via
`askPromptLines` — split on `\n` AND wrapped to width-4; both fixes shipped after real
multi-line workflow-approval cards painted into the same cells / clipped mid-word), then
numbered options (accent ` N `), the typed row (`› {typed}`), legend
`[1-9 pick · ]type an answer · ⏎ send · esc decline`. Height = 2 border + N prompt +
options + typed + legend — App must size `inputH` from the actual line count.

### Help overlay
Full-screen replacement (only surface that displaces everything). A WINDOW, not a page:
`body = rows - 2` (header `keys · esc closes` + position footer), rendered from
`helpLines()` (keymap-derived, cannot drift); offset clamped by the same function the key
handler uses (an unclamped offset blanks the overlay). Rows: blank / header (accent, or
dim when muted) / binding (`  {chord.padEnd(12)}{desc}`, info-colored chord, `  · ` prefix
for prose rows). Footer: `↑↓ pgup/pgdn scroll[ · N more below | · end]`.

### Status line
BELOW the composer (every comparable harness). One dim `meterLine` row: workspace, branch,
model, effort (when set), cost (tree total preferred), context tokens/limit, running
shells/agents/runs counts (these keep live work visible while the panel displaces the
rail), `? help` hint, `←` out-chip only when `originId` exists (chip and binding share the
condition and cannot disagree).

### JobOutput
Buffer split on the RAW `\n` (long lines truncate, never reflow — a scroll offset must
address the same row after a resize); each line keeps only the text after its last `\r`
(progress-bar rewrites: a terminal shows the last segment). Empty buffer → `(no output
yet)` running / `(no output)` else. Status: `⋯ running · 4m12s`; killed-by-signal →
`◼ stopped (SIGTERM) · ran …` (**a signal leaves exitCode null; null-as-zero would paint a
killed shell green**); else `✓ done` / `✗ exit N` + `· ran took`. Footer: armed →
`x again kills it · esc cancels`; else `[N lines below · ]M lines · ↑↓ scroll ·
[x stop · ]esc back`.

### Rail
Each row: `❯` (bold info) when selected; `unitLine(u, w-2)`; hint appended on the selected
row (`⏎ open · x stop · esc composer`, schedules: `⏎ details · x disable · esc composer`)
or the armed row (`x again stops it · esc cancels` / disables). When unselected
(`sel === null`) the last row is the dim kind-counting hint. Rows are
`truncateAnsi → padRow → styled chunks` — the rail redraws every second, exactly where
stale-tail bugs show.

### Panel (chrome + state machine)
See §2 for the reducer. Rendering: outer fixed-height box (rows+2) with border
`palette.border` and background `palette.panel` (a RAISED surface — must be painted, not
transparent); tab strip; then **two nested boxes, both load-bearing**: outer pins height
and clips, inner refuses to shrink. This is the fix for the 100x12 corruption where yoga
shrank six rows into three and pairs of rows interleaved character-by-character
(`❯ ● ✓ wsvewsor28mGreeting Session`); in ratatui the equivalent rule is: **a tab body
must never emit more rows than its budget, and the container must truncate, not scale**.
A zero-row body is not rendered at all (a zero-height clip region clips nothing). Tab
bodies get `bodyCols = max(20, width-4)` (inside border + padding) — legends measured
against full width overran by exactly 4.

### PanelHost (controller) — cursor & entry rules
One cursor (`sel`), reset to 0 on every tab change/open — EXCEPT: the tree lands on the
row where `current` is true (the switcher must land on you-are-here); the model tab lands
on the active (●) row once the settings fetch answers (a `landOnActive` flag, gated on the
config actually arriving — ungated it landed on row 1 and never corrected; `^o ⏎`
silently switched models); `esc esc` lands on the last user turn (`landOn` ref); leaving a
filter keeps the found row (`landOnId` — matched by id because the widened list renumbers).
Arrival also clears: message, diff focus, diff scroll, pending revert, armed stop, wf
drill-in state, wf filter, the `/` buffer.

Entry fetches: model settings (once, when unknown); `store.refreshChanges()`;
`store.refreshWorkflows()`; MCP (`setMcp(null)` then load — never cached); skills (fresh
directory walk; failure sets `null`, NEVER `[]`). The opened workflow run re-fetches on
every `state.workflowSeq` bump.

`handle(command, input)` order (returns true = consumed):
1. `panel.close` while something is drilled/armed → `back()` unwinds ONE level: pending
   revert → armed stop → diff focus → wf level (level 0 also clears wfOpen/detail).
2. `tree.rewind` (esc esc): expand the current conversation, open the tree, compute
   `rewindIndex` against the rows AS THEY WILL BE with it expanded (the closure's `tree`
   predates the expand), set + park the index (the arrival effect doesn't fire when the
   tree is already open). No session → message "no conversation is open — there is no turn
   to go back to".
3. `tree.extract` (`e`): row must be a `message` row (else message "e splits a conversation
   at a TURN — move onto one first"); picks = that turn and all later turns of ITS thread;
   closes panel, `extractFrom`. (Porting trap recorded in-source: `return void f(), true`
   returns `undefined` — the arm must actually return true.)
4. `tree.moveInto` (`m`): mirror direction — copy onto the END of the OPEN conversation;
   local refusals: not a turn / no open conversation / same conversation ("those turns are
   already in this conversation"); the server's three unsound-target refusals land in the
   message row.
5. `panelActionFor` actions: `move` routes per surface — wf levels move
   phaseSel/agentSel/scroll; focused diff scrolls; else `moveTo` (clamps to `items`,
   disarms pending revert + armed stop). `confirm` → `confirm()`; `confirmSummarize` only
   acts on the tree tab (elsewhere it must NOT run the ordinary commit — `s` in the model
   picker used to silently pin a model).
6. Panel-open-only commands: `move.in` (→): wf level < 3 → confirm; changes → focus diff;
   tree session → expand, collapsed → drill in, turn → nothing. `move.out` (←): `back()`
   first; tree session → collapseTurns + collapse; turn/section → collapse its
   conversation. `move.pageUp/Down`: focused diff pages the DIFF (page = bodyRows-2;
   paging used to move the file cursor and silently retarget `x`); else `moveTo ±page`.
7. `panel.pick` (digit 1-9): `pickTargets()` — each tab's OWN exported window function
   called with the same inputs the body renders with (tree: `forestWindow`; model:
   `displayRows`+`modelWindow`+`visibleEntries` — headers/notes not numbered; skills:
   `skillsWindow`; mcp: `mcpWindow`; workflows level 0 only: `wfRunsHeight`+`windowed`;
   changes/theme: none — a digit that jumped-and-affirmed would commit a theme you never
   saw). Digit past the window does nothing (no clamp, no nearest-row). Sets sel AND
   confirms with the index passed explicitly (one gesture).
8. `/` buffer: `panel.filter` opens it (model tab: picks the tier box from the cursor's
   section, effort → frontier); `panel.filterTier` (⇥) swaps boxes (both queries survive);
   `panel.filterBack` (⌫); `panel.filterExit` (esc) — parks the cursor row by id, clears
   the buffer, keeps the panel open (the NEXT esc closes). While filtering, `App` routes
   all text here and the keymap suppresses bare-letter commands (`panelFiltering`), else
   typing "opus" pauses a workflow on the `p`. Tree filter with ≥2 chars also fires a
   debounced (180ms) `store.searchSessions` full-text search; hits are recorded
   `{q, ids, messages}` and each hit conversation is auto-`expand`ed (which fetches its
   thread — a matched turn must be a visible row); hits only feed `forestRows` while
   `searchHits.q` equals the current query.
9. MCP verbs (each guards "not wired into this client" when the thunk is absent):
   `mcp.add` (`n`) — opens the shared buffer as `entryKind: "mcpUrl"` prefilled
   `"https://"`; ⏎ (in `confirm`, BEFORE any tab logic) derives a name via `nameFromUrl`,
   `putMcpServer(name, {url})`, message "registered {name} — a authorizes it, ⏎ grants it
   in every conversation". `mcp.auth` (`a`) — if the entry already carries a static
   credential, first press only warns ("already has a credential (keychain) — press c to
   test it. a again starts a separate OAuth…"); then `beginMcpAuth`: `authorized` →
   done; else the authorizationUrl is PRINTED, never opened (headless servers; the model
   is never handed a URL), the registry re-read (the server may have corrected the URL),
   then `pollAuth`: every 2s up to 150×; on authorized → refresh, auto-CONNECT (tokens
   alone move nothing on screen — the `◐` glyph is about connections) and report tool
   count; timeout → "still waiting on the browser — press a to start over". `mcp.connect`
   (`c`) — proof, not a grant; reports tool count + first 6 tool names or the error.
   `mcp.restart` (`r`) — needs an open conversation (the subprocess lives in its
   checkout); `restarted: false` reads "was not running · the next call starts it", never
   "restarted". `mcp.remove` (`d`) — armed then confirmed; scope out loud (registration +
   grants + stored credentials; the server itself untouched). `mcp.forget` (`F`) — drops
   ONE server's tokens, unarmed (reversible).
10. Workflow verbs (`tab: ["workflows"]`-scoped in the keymap — the old hand-written tab
    guards are gone): `wf.pause`/`wf.resume`/`wf.rerun` → `steer` (target = drilled-in run
    or the cursor row; stop RECORDS to the transcript via `store.record`, pause/resume/
    relaunch stay quiet). `wf.stop` (`x`) — arms first for a running/paused run ("agents in
    flight are lost, and journaled work is kept"); settled runs need no ceremony.
    `wf.script` (`e`) — level 4 (needs an open run; refusal message cleared on success).
    `wf.save` (`s`) — `saveWorkflowAs(wfOpen, workflow.name)` (idempotent by name; no
    prompt); message "saved as \"{name}\" — ask the agent to run it by name · /saved lists
    them". `wf.filter` (`f`) — cycles `WF_FILTERS`, resets agent cursor. `wf.openAgent`
    (`o`) — opens the agent's backing session; no agent under cursor → names the filter as
    the likely reason; `sessionId` absent → "the call was replayed from the journal".
11. Changes verbs: `changes.revert` (`x`) arms the file under the cursor — **a second `x`
    does NOT widen** (the rail teaches `x x` = confirm; widening on it put users one ⏎
    from "revert all", so the all-scope is `X` = `changes.revertAll`, its own key and its
    own arm). ⏎ with a revert pending performs it (`performRevert`): paths =
    `undefined` for all-scope; on success the outcome line (`reverted a, b · not in this
    change set: c · failed d: err`) is set as the tab message AND `store.record`ed
    (a notice expires; deleting a file must leave a permanent transcript mark), cursor
    reset, changes re-fetched. ⏎ with nothing armed toggles diff focus.

`confirm(summarize?, at = sel)` per tab: tree → `selectionFor(row, threads)`: open a
conversation (closes panel) / drill a collapsed fan-out / expand / fork a turn — fork is
addressed to the row's OWN sessionId (any conversation's turn branches that conversation),
user turns cut BEFORE themselves and seed the composer with their text (edit-and-resend),
`summarize` adds `summarizeAbandoned`. changes → pending revert's yes, else toggle diff
focus. workflows → descend one Miller level (0: open run under `at`, reset all sub-state;
1→2; 2→3 only if agents visible; 3: toggle promptOpen). model → `chooseEntry`, message
`MODEL_NOTE`, `store.setModel({model, effort})` with effort `"default"`/null sent as
`null` (the word "default" must not be pinned). mcp → grant/revoke toggle: revoke arms
first (⏎ used to land here from text typed into a panel that looked like the composer and
revoked a server install-wide); the write is GLOBAL (sessionId `""` = every session —
per-conversation grants made every new conversation start dark). theme → commits in the
reducer; skills → nothing to affirm.

`scrollBy` (wheel): only `changes` + focused diff consumes.

The rendered view passes the same `message`/`sel` into whichever tab body; tree and
workflows arrive as children and receive `bodyRows` (NOT `body` — Panel can't subtract
chrome for children) and `cols-4`; workflows loses one more row to its message line.

### Tree tab
Row shapes: session rows `[cursor]▸/▾ glyph mark title [⋯N] [N running] [dir] [$cost]` —
disclosure first (the door mark), delegated kinds undimmed vs dim kind glyph, current row
green+bold, cross-workspace dir shown only at depth 0 when different, `busyBelow` named in
cyan ("look inside" vs "leave it alone"); message rows `├─/└─ role gist [◂ match]
[← active]` (role labels: user→"you" white, supervisor→"bough" green, system yellow; gist
clipped `max(12, 54-2·depth)`); section rows are dim captions `── label` (not actionable);
collapsed rows `⋯ N spawned · → drill in`. Empty list: filtered → `nothing matches "{q}" —
titles, paths or messages` (says WHAT was searched); else "no conversations yet". Legends
(2 rows, always last): `markLegend(window)` then keys `[{sel+1}/{n}] ↑↓ move · →← turns ·
⏎ open · ⏎ on a turn forks · e splits · m brings here · / find · esc back`; then the
message row when present (each takes a chrome row from the window).

### Changes tab
`set === null` → "loading changes…"; `!available` → the server's `reason` (warn, word-wrap)
+ hint (suppressed via `hint: null` for the no-session case — the non-git sentence is
false when there is no checkout) + `esc back · ^t close`. Budget is COUNTED: message row,
file list (header `N files changed [since {base8}]` + up to `min(6, …)` rows), diff (blank
separator + body + `— n/m —` overflow marker), footer (3 rows when a confirm is pending,
else 1 legend row); the confirm takes rows FROM THE DIFF (a confirm scrolled off is a
confirm nobody read). Focus mode replaces the list with one `M path` header and gives the
tab to the hunks. Diff lines: `@@` info, `+` accent, `-` error, context dim. Legends:
focused → `← back · ↑↓ scroll the diff · x revert this path · X revert everything`; list →
`↑↓ move · → focus one file · x revert this path · X revert all · esc back`. Confirm
wording: file scope headline `revert {path}?` + `revertScope` sentence + `⏎ revert it
[· X all N files] · esc cancel`; all scope (error color) `revert all N files (+a -r)?` +
"everything this session touched goes back[ to {base8}], and files it created are deleted"
+ `⏎ revert everything · esc cancel`.

### Model tab
DisplayRows windowed with the cursor mapped through entry index. Entry row:
`{ordinal ≤9} ❯ ● label  detail` — the printed ordinal counts entries only, skipping
headers, matching `visibleEntries` exactly. `●` marks the in-force row per tier; cheap tier
unset gets the `CHEAP_UNSET` note row (a real state, not a missing dot). Search boxes are
DisplayRows (counted by the window; a row painted outside the count clips into garbage) and
render under their own section; an empty-matching section keeps header + box + `NO_MATCH`.
Legend: filtering → `narrowing {tier} · tab other box · ⌫ back · esc clear · ↑↓ move · ⏎`;
else `↑↓ move · pgup/pgdn page · 1-9 pick · / search this section · ⏎ choose · esc back`.

### MCP tab
Row: `{digit} ❯ ●/◐/○ name  {mcpDetail}` (alive accent / granted warn / off dim). URL-entry
prompt row `new server {entry}▌` replaces the affirmative while open. A message containing
`://` word-wraps un-clipped (a truncated auth URL is useless); other messages clip at 96.
Empty registry: `no MCP servers configured — n adds one by URL` with a 2-key legend (a
legend listing inert keys loses trust). Overflow: `— end/N —` counter. Legends: mark
legend (present-only), then keys `↑↓ move · 1-9 pick · ⏎ grant/revoke · c test · r restart
· a authorize · n add · F forget · d delete · esc back` (or `⏎ registers · ⌫ back · esc
cancels` while the URL buffer is open). `legendLine` drops whole items when narrow.

### Skills tab
`/name  description` rows (broken skill: error color, its parse `error` in place of the
description); `mcp: a, b` info tail. Filter row `/{q}▌` while typing. Counter
`{at+1}/{N} · ↑↓ to see the rest`. `read from {source dir · …}` row above the legend.
Description clip adapts to `cols` (was hardcoded 60).

### Theme tab
Rows: `❯ name.padEnd(16) [swatch cells] note`. Cursor movement = live preview of the WHOLE
TUI (the palette is a process-global; the transcript memo keys on `palette.epoch`); ⏎
commits (persists via injected writer); ANY departure reverts (reducer-owned). The preview
object is created once per TUI process (not per tab entry) so the baseline `cancel()`
restores is the real theme, seeded from the server's boot state.

### Workflows tab
Level 0: `{digit} ❯ glyph name  description  done/total[ · N replayed][ · N failed] ·
elapsed`. Levels 1–3: clipped header + Miller columns; left pane = phases (or the compact
agent list once an agent is open, retitled to the phase), right pane = the phase's agents
(full rows) or the open agent's detail; column titles dim; right title shows
`{phase} · {shown}[ {filter}]`. Level 4: header + numbered script. Elapsed format
`12s` / `3m07s` — seconds survive past a minute (a wedged agent shows on the clock).
`detail.live == false` on a running run renders `(not live here)` warn and steering offers
only stop+script (a pause it cannot honor is not offered). Footers per §2 `footer()`;
every one ends `esc back`; `legendLine` degradation keeps the tail pinned.

### Misc traps a naive port WILL get wrong
- **Burst semantics**: anything a key handler reads that another key in the same stdin read
  may have changed must come from the current mutable state, not a stale snapshot (TS used
  refs: `lineRef`, `selRef`, `quitArmedRef`, `pastedRef`, `runRef`). In Rust with a single
  event loop and `&mut` state this collapses to "just mutate one struct" — but keep the
  ORDER: e.g. a paste's ordinal is claimed at handle time, and two pastes in one batch must
  get distinct ordinals.
- **Numbers must be computed once and shared**: `chatBodyHeight`/`lineAtSlot` between
  renderer and hit-test; each tab's window function between renderer and digit resolver;
  `composerHeight` between the composer and the frame arithmetic; `tabAtColumn` walks the
  exact strip widths. Duplicate arithmetic = clicks/digits one row off.
- **Row budgets are claims**: floors like `max(3, …)` when only 1 row exists caused the
  interleaved-row corruption; in ratatui always clamp emitted rows to the Rect height.
- Theme change must invalidate the built transcript (SGR is baked into `VLine.text` at
  build time; palette epoch is the memo's first dependency).
- Selection is stored in SCREEN coordinates deliberately (a transcript-anchored selection
  slides when output streams underneath).
- `session.copyId` reads current state, not the state captured when the binding was built.
- `!` history entries keep their `!` sigil; slash invocations and shell commands never
  enter the thread.
- `expandPastes`: a paste whose mark was deleted from the draft is dropped from the message.
- `outcomeOk === false` outranks `lastTurnStatus: "done"` (the lying-checkmark family: also
  `runGlyph`, tree `busyBelow`).
- OAuth: never open a browser; print the URL; poll bounded; on success auto-connect.
- Revert: `paths: undefined` = whole set; `[]` is refused by the server.
- Job kill status: `signal` present ⇒ stopped, even with `exitCode: null`.

---

## 5. Dependencies

Imports (within `src/tui/`): `lines.ts` (buildLines, messageLines, chatBodyHeight,
visibleSlice, lineAtSlot, branchesFrom, VLine), `keys.ts` (chordOf, lookup, isTextInput,
editLine, insertText, stripCtl, helpLines, SLASH_COMMANDS, slashInvocation, unknownCommand,
UNSEND_MS, TABS/PANEL_TABS/tabForCommand/tabForChord, Command, UiMode, KeyContext,
LineState, EMPTY_LINE), `store.ts` (Store, TuiState, currentAsk, isBusy, liveUnits,
marksFor, LiveUnit), `format.ts` (activeTrigger, rankCompletions, applyCompletion,
browsePrefix, wrapLine, clip, plural, meterLine, busyLine, unitLine, legendLine,
windowAround, fuzzyScore, ansiSpans, truncateAnsi, width, linkAt, urlAcross, sessionLabel,
shortenPath, fmtUsd, fmtTokens, fmtDuration, oneLine, UI colors, SPINNER_MS), `forest.ts`
(forestRows, selectionFor, rewindIndex, revealPath, takeBackTarget, isCollapsed,
isDelegated, DELEGATED_KINDS, ForestRow, ForestInput), `theme.ts` (palette, subscribeTheme,
themeEpoch, createThemePreview, presetSwatch, ThemePreview/Preset/State), `api.ts` (api
client + SessionRow, WorkflowSummary/Detail), `selection.ts` (Selection, selRows, rowSpan,
rowContent, selectedCopy, isEmptySelection), `paste.ts` (expandPastes, pasteMark,
QUEUE_ABOVE_CHARS), `clipboard.ts` (clipboardImagePath), `mouse.ts` (MouseEvent, NavKey
types). Cross-tree type-only imports: `schema/parts.ts` (Message, SessionKind,
BackgroundJob, WorkflowRun), `llm/client.ts` (ModelRow), `server/changes.ts`
(SessionChangeSet), `server/skills.ts` (SkillRow), `vcs/repodiff.ts` (FileDiff),
`mcp/status.ts` (McpStatus), `mcp/keychain.ts` (isCoveredHost — the ONE runtime cross-tree
import; consider re-exporting through the API layer in Rust), `workflow/control.ts`
(WorkflowAgentView), `workflow/report.ts` (ReplaySummary, RunCost, LargeRunFlag).

Imported by: `tui/main.tsx` (mounts App, supplies InputHooks/controls/models/theme/
transport) and the component tests. Nothing outside `tui/` imports components.

## 6. External deps → Rust equivalents

| TS dependency | Used for | Rust replacement |
|---|---|---|
| `@opentui/core` / `@opentui/react` (box/text/StyledText, useKeyboard, useRenderer, useTerminalDimensions) | rendering, layout, key events, resize | **ratatui** + **crossterm** (events incl. mouse + bracketed paste + resize); layout via `Layout`/manual Rect math — the TS code already computes exact row budgets by hand, which ports directly |
| React (`useState`/`useMemo`/`useEffect`/`useSyncExternalStore`/refs) | state + derived data + subscriptions | a plain `AppState` struct mutated by an event loop; memos become plain recomputation or `(inputs-hash, value)` caches for `buildLines`; effects become explicit calls at the mutation site; store subscription = `tokio::sync::watch`/`broadcast` |
| `slice-ansi` | ANSI-aware substring for the selection highlight | slice over parsed spans (own `ansi_spans` port); or `ansi-cut` crate |
| `strip-ansi` | plain text of highlighted rows | `strip-ansi-escapes` crate |
| `setInterval`/`setTimeout` timers (spinner, rail poll, job poll, branch poll, ghost debounce, search debounce, esc hold, OAuth poll) | clocks | `tokio::time::{interval, sleep}` tasks sending messages into the event loop; the esc-hold is a cancellable sleep handle |
| `fetch` via `api.ts` | REST | `reqwest` (or the workspace's shared client crate) behind the same injected-thunk trait |
| `Blob` (clipboard image) | image attachment upload | `Vec<u8>` + mime string |
| `URL` (nameFromUrl) | hostname parsing | `url` crate |
| `TextDecoder` + renderer buffer readback (`screenRows`) | copy-from-anywhere fallback | ratatui `Buffer` readback: iterate the previous frame's `buffer.content` cells per row — strictly easier than the OpenTUI hack |
| display width (`format.ts::width`) | CJK/emoji-safe padding | `unicode-width` crate |
| OSC 8 / OSC 52 (links, clipboard — in term.ts but exercised here) | clickable links, copy | write escapes via crossterm `execute!`; OSC 52 manually |

## 7. Suggested Rust layout

```
bough-tui/src/components/
  mod.rs            // pub use; the Frame enum (Chat|Panel|Job|Help)
  app.rs            // AppState + update(Event) -> (); frame layout arithmetic; run(Command)
  submit.rs         // submit(), the !/slash/unknown-command ladder (pure over AppState + effects trait)
  chat.rs           // Chat widget: fn render(f, rect, &ChatProps); ChatMeter
  message.rs        // pad_row, styled_row (ansi spans -> Line<'_>), MessageRow
  composer.rs       // composer_height, completion_popup_height, render; CompletionPopup
  ask.rs            // ask_prompt_lines + AskCard
  help.rs           // Help overlay + clamp_help_offset
  job_output.rs     // job_sub_lines, job_body_rows, render
  rail.rs           // live_subagents, rail_hint, render
  panel/
    mod.rs          // PanelState, PanelAction, panel_action_for, reduce_panel (theme revert), tab strip + tab_at_column, panel_body_rows, Panel chrome render
    host.rs         // PanelHostState: cursor, filters, wf drill-in, mcp/skills caches, entry fetch triggers, handle(), confirm(), pick_targets(), name_from_url
    tree.rs         // kind_glyph, status_mark, title_of, forest_window, mark_legend, render
    changes.rs      // ChangeItem, file_stats, change_items, diff_body, PendingRevert, revert_scope, render + confirm card
    model.rs        // ModelConfig, EffortChoice, modelEntries/display_rows/model_window/visible_entries/choose_entry, render
    mcp.rs          // mcp_window, has_static_auth, mcp_detail, status_legend, render
    skills.rs       // skills_window, render
    theme_tab.rs    // render (preview object lives in tui/theme)
    workflows.rs    // Tone/Cell/Row, wf_glyph/run_glyph, phase_groups, visible_agents, replay/cost/warning rows, steer_actions, header/phase/agent/agent_detail/script rows, footer, wf_runs_height, render, WorkflowChip
```

Traits & boundaries:
- `trait PanelControls` (or a struct of `Box<dyn Fn… -> BoxFuture>`) for the injected REST
  thunks — keeps host testable with fakes, exactly as the TS does. Same for the App-level
  transport (`copy_text`, `open_url`, `notify_desktop`, `upload_image`, clipboard hooks).
- Pure row builders (everything in Workflows above the component, Tree's helpers, Changes'
  fold, ModelPicker's entries) are free functions over the wire types returning
  `Vec<Row>`/strings — port them first, they carry the test suite.
- Async boundary: ONE tokio event loop consuming an mpsc of
  `Event::{Key, Mouse, Paste, Resize, Tick, StoreChanged, FetchDone(FetchKind, Result)}`.
  All fetches are spawned tasks that post `FetchDone` with a generation/session guard
  (the TS `alive` flags) so a stale response for a switched session is dropped. Timers are
  tasks posting Ticks at the current rate; recompute the rate after every state change
  (busy > live > schedules > none).
- Rendering is synchronous and pure over `&AppState` each frame; the store snapshot is
  cloned/Arc'd in. `buildLines` result cached on (thread version, palette epoch, cols,
  fold state, …) as in TS.

## 8. v1 scope cut

Must exist for a working loop (core): App frame layout + key dispatch + submit (plain send,
`!` shell, slash invocation, unknown-command refusal), Chat + Message row hygiene, Composer
(draft, cursor, wrap/cap, busy hint), StatusLine, SubagentRail + rail keys, JobOutput +
poll, Panel chrome + reducer + PanelHost with the tree/changes tabs, Tree (open/expand/
drill/fork via ⏎), Changes (list/diff/armed revert), Help overlay, esc semantics
(interrupt/hold/double-tap), scroll everywhere.

Stub or defer initially (each degrades silently by design):
- **Ghost text, activity blurbs, topic sections** — cheap-tier cosmetics; every failure
  path is already silence. Stub = never fetch.
- **Mouse**: drag-select + screen-readback copy and link clicking are self-contained; v1
  can ship wheel-scroll + click-to-fold only (or keyboard-only) without touching anything
  else. Keep `clickAt`'s row arithmetic when added.
- **Image attachments + clipboard-path detection** — needs upload plumbing; text pastes and
  held-paste marks are the load-bearing half and are cheap.
- **Model/Skills/Theme/MCP tabs** — panel tabs are additive by construction (Panel's whole
  invariant). MCP is the largest (OAuth poll, add-by-URL, 7 verbs) and can land last.
  Theme needs the palette-epoch plumbing; defer with a fixed palette.
- **Workflows tab levels 1–4** — level 0 list + stop/pause suffices to steer; the Miller
  drill-in, script view and save are pure-row work that ports mechanically later. The
  replay-accounting rows must NOT be cut once the detail view exists (spec §8).
- **Session surgery**: extract (`e`), move-into (`m`), summarize-fork (`s`), take-back
  (`message.unsend`) — each is one arm + one API call; defer behind "not wired" messages,
  which is the codebase's own idiom for absent capability.
- **Draft persistence over REST, sections, search-in-tree FTS, branch poll** — quality-of-
  life; stub cleanly.
Do NOT cut: row padding/truncation hygiene, exact shared window arithmetic, the two-press
destructive idiom, the not-a-repo vs empty distinction, live-work pinning, fetch-first job
open, ref-vs-burst ordering.
