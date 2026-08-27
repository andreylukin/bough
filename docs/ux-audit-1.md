# bough rebuild TUI (Phase 4) — persona usability audit #1

10 personas, ~10 scripted steps each, driven through `shell-use` at 120x36 with SVG capture.
Personas: developer-critic, power-user, keyboard-only-user, low-vision-user, cognitive-adhd-user,
busy-executive, designer-critic, boomer-tech-averse, non-native-english, andrey-owner.

## 1. Executive summary

1. **The engine is fine; the surface loses user data.** Every persona reached a working agent that wrote a real file, showed a real diff, and answered follow-ups. Nine of ten still said they would not use it.
2. **One root cause dominates: there is no focus model.** Clicking anything in the transcript (the thing you do to expand a tool call) silently moves keyboard focus off the composer, with the composer's cursor and placeholder still rendered as if live. 10/10 personas lost at least one typed-and-sent message this way.
3. **The same missing focus model breaks scrolling.** Scroll keys work only when focus is in the transcript, the wheel only when it is not, and nothing follows new output. 10/10 personas got stranded in history while turns ran and answered off-screen; three never recovered without a lucky mouse click.
4. **Typed text is destroyed in three more ways:** any sentence starting with `/` (10/10), a raw multi-line paste with no bracketed-paste wrapper (4/10 — all but the last line vanishes), and Esc on a non-empty draft.
5. **The frame paints over the content.** The status strip and a hardcoded 34-column rail share baselines with the transcript, so row 0 literally reads `○ sol   idle- push_to_pr — Add commits…`. 9/10 personas read it as terminal corruption.
6. **Streaming is rendered per network chunk, not per word.** Line breaks land mid-sentence and mid-word (`clear distin`/`ctions`), markdown spans are shredded across chunks, and the breaks are *persisted* — they survive quit and relaunch. 10/10 hit it.
7. **Every power surface is skeleton-only.** Search dumps raw ledger JSON and its hits are inert; `/help` advertises key hints and lists none; `/` opens no palette despite the placeholder promising one; four documented commands are no-ops; a rejected `bough.patch.yml` logs a perfect error to `bough.log` and shows nothing on screen.
8. **The layout wastes a third of the terminal in both axes** — a fixed 34-column rail at 80 *and* 200 columns, plus a bottom pane that reserves ~10 rows whether or not it has content, and panels that never dismiss.
9. **Contrast hierarchy is inverted.** Body prose is ~8.9:1; `/help` and *every error message* are #565f89 on #282d35 ≈ 2.1:1 — the instructions and the failure reports are the least legible text in the product.
10. **What to fix first, in order:** the focus model (`tui-focus`), scroll + jump-to-latest (`tui-shell`), the three text-destruction paths, then the strip/transcript gutter. Everything else is polish on genuinely good bones — the tool-row disclosure, the diff renderer, turn markers, message queueing and history persistence were praised by name in 9/10 walks.

## 2. Prioritized deduped findings

39 findings after merging: **8 blockers, 20 majors, 6 minors, 5 nits.**
`personas` = how many of the 10 walks independently hit it. Severity is the highest reported unless noted.

| # | Sev | Personas | Title | Repro (one line) | Proposed fix (crate) |
|---|-----|----------|-------|------------------|----------------------|
| 1 | blocker | 10 | Clicking the transcript silently kills the composer; typed-and-sent messages vanish | `mouse click --on-text "write_file notes.txt"` → `type "hello test"` → Enter → nothing echoes, nothing sends | One always-live input; if focus can move, draw a focus ring and snap back on any printable key — **tui-focus** |
| 2 | blocker | 10 | Scroll is focus-dependent and the view never follows new output; no jump-to-latest | PageUp ×10 → send a message → PageDown ×30 / End / wheel-down: view never moves | Focus-independent scroll keys + auto-follow at tail + `End`/"↓ N new" affordance — **tui-shell** |
| 3 | blocker | 10 | A message starting with `/` is eaten as an unknown command and the text is destroyed | `type "/tmp is where my files are"` → Enter → `` unknown command `tmp` `` and the sentence is gone | Keep text in the composer on command miss; offer "send as message"; support `//` escape — **commands** |
| 4 | blocker | 4 | Raw (non-bracketed) multi-line paste fires N sends and drops all but the last line | `shell-use write "$(printf 'alpha\nbeta\ngamma')"` → composer holds only `gamma`; alpha/beta never sent | Treat a fast newline burst as a paste; buffer into one draft — **tui-shell** |
| 5 | blocker | 1 | Tools ran in `/Users/andrey/repos/bough`, not the launch cwd, and the agent said "the current directory" | Launch from an empty cwd → "create notes.txt in the current directory" → file lands in the bough repo | Inherit and pin the process cwd for tool exec; render cwd in the strip — **agent-loop** |
| 6 | blocker | 2 | Tool-call rows are mouse-only — no keyboard path to expand a diff | Up/Down/Tab/Space/Enter over `▸ write_file notes.txt`: nothing selects, nothing expands, no focus ever shown | Roving focus over transcript rows with a visible indicator; Enter/Space toggles — **tui-focus** |
| 7 | blocker | 2 | A second Ctrl+C quits the app instantly with no "press again to exit" | long prompt → Ctrl+C (interrupts, no visible confirmation) → Ctrl+C again → process dies, PTY I/O error | Confirm-to-exit hint on idle Ctrl+C; visible `interrupted` line on the first — **tui-shell** |
| 8 | blocker | 2 | `/quit` blanks the terminal with no goodbye, and once never exited at all | `type "/quit"` → Enter → solid black screen, zero characters; process still alive after 20s | Print a one-line farewell, restore the terminal, exit promptly with a timeout — **tui-shell** |
| 9 | major | 9 | Status strip and left rail paint on the same baselines as the transcript — no gutter | Send any answer >20 lines; read row 0: `○ sol   idle- push_to_pr — Add commits…` | Reserve the strip row; one-column gutter or rule between rail and transcript — **tui-strip** |
| 10 | major | 10 | Streaming chunk boundaries are baked in as hard line breaks, mid-sentence and mid-word | Ask any long prose question; read `I'll create` / ` a file named notes.txt`, `clear distin` / `ctions` | Accumulate the stream, wrap on paint at word boundaries — **tui-shell** |
| 11 | major | 10 | Search results are raw ledger JSON; hits are inert or jump to the wrong place; no highlight, no count | Ctrl+F → `capabilities` → `sol s8 thought/text  {"step_index":0,"text":"! I'm **sol**…` | Index rendered conversation text; snippet + highlight + n/N + count — **tui-search** |
| 12 | major | 10 | `/help` and search panels never dismiss and reserve ~10 rows even when empty | `/help` → Enter → Escape → panel persists through every later turn and a resize | Esc dismisses the topmost overlay; panels size to content — **tui-shell** |
| 13 | major | 10 | The rail is a hardcoded 34 columns at every width; history never re-wraps on resize | `resize 80 24` (rail still 34 of 80) then `resize 200 50` (rail still 34, still ellipsized) | Collapse the rail under ~100 cols, re-wrap on resize, cap prose measure at ~80–90 — **tui-strip** |
| 14 | major | 10 | Esc does not interrupt a running turn, and nothing on screen names the key that does | Send a long prompt → Escape ×3 (no change, no feedback) → Ctrl+C (interrupts) | Bind Esc to interrupt; show `esc to interrupt` in the running strip — **tui-shell** |
| 15 | major | 10 | Config patch accept/reject is invisible — the error exists only in `bough.log` | Write `model.policy.sol` into `$BOUGH_HOME/bough.patch.yml` → screen unchanged; `tail bough.log` shows the WARN | Surface reload/reject as a strip notice with the same text the log gets — **residents** |
| 16 | major | 10 | Empty first launch has no product name, cwd, model, version, or help hint; `?` types a `?` | Launch with an empty `BOUGH_HOME`; read the screen; press `?` | First-run strip: name, cwd, model, `? help`; complete the placeholder sentence — **tui-shell** |
| 17 | major | 10 | Typing `/` opens no command palette although the placeholder advertises one | `type "/"` → wait → nothing; `type "he"` → still nothing; Tab does not complete `/ag` | Filtering command menu on `/` at line start, Up/Down + Enter/Tab — **commands** |
| 18 | major | 10 | `/help` promises "key hints", lists zero; descriptions are internal metaphor | `/help` → Enter → 8 slash lines, no keys; `/quit  tear the tree down and leave` | List real bindings (Ctrl+F, Ctrl+C, PageUp/Dn, End); plain-language descriptions — **commands** |
| 19 | major | 8 | Markdown is half-rendered: literal `##`, `**`, backticks and table pipes on screen | Ask "what can you do here?" → `## Core Capabilities`, `- **Work` / ` with branches**`, `\|----------\|` | Parse markdown over the accumulated document; render or strip consistently — **tui-shell** |
| 20 | major | 9 | Readline gaps: Ctrl+U deletes one char, no Up-arrow history, Tab/Ctrl+L dead, Shift+Enter unreliable | `type "abcdefgh"` → `keys Ctrl+u` → `abcdefg`; Up on an empty composer recalls nothing | Full readline set + sent-message history + Shift/Alt+Enter newline — **tui-shell** |
| 21 | major | 9 | No selection or copy — and the mouse grab disables the terminal's own selection | `mouse drag 34 10 110 10` → no highlight, no toast, no clipboard change | In-app select + "copied" flash, or release the grab / hint Shift-drag — **tui-shell** |
| 22 | major | 8 | Contrast hierarchy inverted: help and every error at ~2.1:1, chrome at ~3.0:1 | `/help` then `/nonsense`; inspect fills: `#565f89` and `#6f7783` on `#282d35` | Errors in a warning hue ≥4.5:1; help at body contrast; chrome ≥4.5:1 — **tui-strip** |
| 23 | major | 7 | Mouse wheel does not scroll the transcript (contradicted by 1 persona — focus-dependent) | `mouse move 80 15` → `mouse scroll up --amount 8` → screen byte-identical | Wire wheel to the transcript viewport unconditionally — **tui-shell** |
| 24 | major | 4 | The status strip carries no model, cost, context-left, or cwd — 100 of 120 columns blank | Launch and read row 0: `○ sol                         idle` | Model + cwd + %context + cost in the strip — **tui-strip** |
| 25 | major | 4 | The agent advertises tools it does not have (`open_pr`, `linear_write`, `spawn_worker`) | Fresh home → "what can you do here?" → a capability list nothing in the UI corroborates | Ground the capability answer in the tools actually registered — **agent-loop** |
| 26 | major | 1 | Click hit-test is off by one row; clicking an expanded `▾` row never collapses it | `mouse click 50 11` while `write_file` renders at row 12 → row 12 expands | Fix row hit-test origin; make `▾` rows toggle — **tui-focus** |
| 27 | major | 1 | Four of eight documented slash commands are silent no-ops | `/focus sol`, `/drift`, `/oldfeed`, `/prime unix` → zero visible change (`bough.log` explains `/oldfeed`) | Every command renders output or a reason it can't — **commands** |
| 28 | major | 1 | History not restored on relaunch (contradicted by 8 personas — likely a flush-on-quit race) | `/quit` → relaunch same `BOUGH_HOME` → empty transcript, Ctrl+F finds nothing, 231k WAL on disk | Flush/checkpoint the ledger on shutdown; verify against the `/quit`-hang path — **residents** |
| 29 | minor | 9 | Rail about-line and per-turn summary are semicolon-spliced fragments, truncated mid-word, blank after relaunch | Send "say hi"; read rail rows 2–3: ``read mail `say hi`; Hi; ! 👋 ; **`` | One clean sentence, markdown stripped, persisted with the session — **residents** |
| 30 | minor | 8 | The search field is an unlabelled dim string floating mid-screen and never clears | Launch → `search /` alone at row 24; Ctrl+F, type, Escape → query and results stay all session | Show on invoke, give it chrome, clear on Escape — **tui-search** |
| 31 | minor | 6 | `/agents` prints one unlabelled bottom-anchored row; the command is never echoed | `/agents` → Enter → `sol   idle   lane/sol   0 queued` at row 35, 7 blank rows above | Column headers, top-anchored, echo the command — **commands** |
| 32 | minor | 3 | No progress affordance beyond one small word changing colour | Send any prompt and watch: only `○ sol idle` → `● sol running` in the corner | Spinner/elapsed near where the answer lands + `esc to interrupt` — **tui-strip** |
| 33 | minor | 1 | Clicking the composer always places the cursor at position 0 | Text `Prior` in composer → `mouse click 40 35` past the text → Backspace does nothing, typing lands at the front | Map click x to character offset — **tui-shell** |
| 34 | minor | 1 | Wrapped list items have no hanging indent; two indent metrics in play | `resize 95 30`; ask for a numbered list with long items → continuation flush with `8.` | Hang-indent to the text after the marker; one indent step per level — **tui-shell** |
| 35 | nit | 5 | Diff/tool-output styling: grey `+` lines, neutral `✓` 80 columns from the name, opaque `@@`, exit status twice, inconsistent containers | Expand `write_file` then a `bash` row; compare containers and fills | Tint whole diff lines, colour the glyph, one container for all tool output — **tui-shell** |
| 36 | nit | 4 | `unknown command` offers no did-you-mean and no pointer to `/help` | `type "/nonsense"` → Enter | ``unknown command `nonsense` — try /help`` + nearest match — **commands** |
| 37 | nit | 4 | User-facing chrome uses internal vocabulary: "wake", "mail", "lane/sol", "intent (self-declared)" | Send a message; read the turn markers and the rail | Rename to turn/message in user-facing surfaces — **tui-strip** |
| 38 | nit | 1 | No speaker differentiation, no timestamps; queued messages render as an undifferentiated block | Send two messages back to back → `andrey:` / text / `andrey:` / text with no rule or spacing | Distinct user-turn treatment + relative timestamps — **tui-shell** |
| 39 | nit | 1 | Reflow after resize injects spurious blank lines | Two short messages → `resize 100 30` → `resize 140 40` → `ONE` / blank / blank / `TWO` | Preserve spacing across reflow — **tui-shell** |

## 3. Blocker and major detail

### B1 — Clicking the transcript silently kills the composer (blocker, 10/10)

**What happened.** Clicking a tool-call row to expand it works, and then the composer stops accepting input. Every persona typed a full message and pressed Enter into a void: no character echoed, no turn started, the status stayed `idle`, and the composer kept drawing its cursor block and its placeholder `message, or / for a command` exactly as when live. Losses were substantial — 116 characters over two messages (keyboard-only), a 96-character question (designer-critic), two full prompts (power-user). Tab produces the identical dead state. Recovery differs per persona: Escape worked for some, only a click on the composer line for others; nobody found it deliberately.

**Expected.** Expanding a disclosure row does not move keyboard focus. If focus can move, the focused region carries a visible ring/tint, the composer visibly goes inert (greyed cursor, dimmed field), any printable key snaps focus back, and Escape reliably returns to the composer.

**Repro.**
```
shell-use mouse click --on-text "write_file notes.txt"
shell-use type "explain how git rebase works in detail"
shell-use press Enter
shell-use text | sed -n 36p     # still the placeholder; no turn started
shell-use press Escape          # or: shell-use mouse click 20 35
```

**Screenshot.** `ux/andrey-owner/shots/04b-lostinput.svg` (also `ux/designer-critic/shots/04b-lost-input.svg`, `ux/cognitive-adhd-user/shots/04-lost-input.svg`)

### B2 — Scroll is focus-dependent and the view never follows new output (blocker, 10/10)

**What happened.** After paging up, the transcript stops responding. Which keys are dead depends on where focus landed, which is why the walks contradict each other: designer-critic found PageUp/PageDown/Home/End all no-ops with the wheel as the only working scroll; seven others found the wheel dead and PageUp the only working scroll; boomer and cognitive-adhd found *nothing* worked. Nobody found `End` or any jump-to-latest. Meanwhile turns keep running: developer-critic sent two more turns and watched the rail update while the main pane stayed on old text; cognitive-adhd sent six messages from a stranded view and found them absent after a restart; boomer and non-native concluded the program had died. There is no scrollbar, no position indicator, and no unread marker.

**Expected.** PageUp/PageDown/Home/End and the wheel scroll the transcript regardless of focus. The view auto-follows while pinned at the tail. When it is detached, a persistent affordance says so (`↓ N new`) and `End` always returns to live. Sending a message returns to the tail.

**Repro.**
```
shell-use press PageUp        # ×10
shell-use type "say THREE"; shell-use press Enter
shell-use press PageDown      # ×30 — view never moves
shell-use press End; shell-use press Home
shell-use mouse scroll down --amount 20
shell-use mouse click 80 10   # only this unsticks it (and kills the composer, see B1)
```

**Screenshot.** `ux/designer-critic/shots/08e-new-msg-frozen.svg` (also `ux/andrey-owner/shots/11d-stuck.svg`, `ux/cognitive-adhd-user/shots/11e-after-restart.svg`)

### B3 — A message starting with `/` is destroyed (blocker, 10/10)

**What happened.** `/tmp is where my files are` produces `` unknown command `tmp` `` and deletes the entire sentence. The text is not returned to the composer, there is no history recall to get it back, no `//` escape is documented, and no "send as a message?" is offered. For low-vision-user this compounded: the error rendered at #565f89 (2.24:1) eleven blank rows below where they were looking, so the message appeared to vanish with no explanation at all.

**Expected.** A command miss leaves the typed text in the composer, or offers "no such command — send as a message?", or an escape (`//`) is documented. Nothing typed is ever deleted because of its first character. The error renders in a warning hue next to the composer.

**Repro.**
```
shell-use type "/tmp is where my files are"
shell-use press Enter
shell-use text | sed -n 36p     # empty; sentence unrecoverable
```

**Screenshot.** `ux/developer-critic/shots/06c-slashmsg.svg` (also `ux/low-vision-user/shots/07a-80x24.svg` for the contrast/distance compound)

### B4 — Raw multi-line paste fires N sends and drops all but the last line (blocker, 4/10)

**What happened.** A paste that arrives without the bracketed-paste wrapper is destroyed. busy-executive pasted `alpha`/`beta`/`gamma` and only `gamma` reached the composer — the first two lines were consumed and never appeared as sent messages. designer-critic pasted four lines; each newline fired as Enter and only `line three` was recorded as a turn. cognitive-adhd got three separate sends. developer-critic isolated the boundary: raw bytes are swallowed entirely, while the identical bytes wrapped in `ESC[200~…ESC[201~` work perfectly (confirmed as a delight by six other personas). Terminals that do not advertise bracketed paste silently lose the user's text.

**Expected.** A fast newline burst is treated as a paste and buffered into one multi-line draft (ideally collapsed to a `[pasted 4 lines]` chip); Enter sends it once.

**Repro.**
```
shell-use write "$(printf 'alpha\nbeta\ngamma')"   # composer holds only "gamma"
shell-use write $'\x1b[200~alpha\nbeta\ngamma\x1b[201~'   # works correctly
```

**Screenshot.** `ux/designer-critic/shots/11e-paste-result.svg` (also `ux/busy-executive/shots/11c-paste-result.svg`)

### B5 — Tools ran in the wrong cwd and the agent claimed otherwise (blocker, 1/10)

**What happened.** developer-critic launched with `cwd=…/ux/developer-critic/cwd`, asked for `notes.txt` "in the current directory", and got `▸ write_file notes.txt ✓` plus "created … in the current directory". No such file existed in their cwd. A follow-up `pwd && ls -la` reported `/Users/andrey/repos/bough`, and `git status` in the real repo showed `?? notes.txt`. Six other personas verified their file landed correctly, so this is a launch-path-specific cwd inheritance bug rather than a blanket one — but a coding agent that misreports its cwd and writes into an unrelated git repo is stop-ship regardless of frequency. Nothing anywhere on screen names the working directory (see also M24), so there is no way for a user to catch it.

**Expected.** Tool calls execute in the process cwd, and the cwd is displayed persistently in the status strip.

**Repro.**
```
mkdir -p /tmp/scratch-cwd && cd /tmp/scratch-cwd
shell-use run --cols 120 --rows 36 -- bough
shell-use type "create a file named notes.txt in the current directory with three lines"; shell-use press Enter
shell-use type "use bash to run: pwd && ls -la"; shell-use press Enter
```

**Screenshot.** `ux/developer-critic/shots/03d-wrong-cwd.svg`

### B6 — Tool rows are mouse-only (blocker, 2/10)

**What happened.** Tool rows render `▸ write_file notes.txt   ✓` with a disclosure triangle. keyboard-only-user (a mouth-stick user) pressed Up, Down, Tab, Space and Enter in turn: nothing selected, nothing expanded, and no focus indicator appeared anywhere on screen at any point. Only `mouse click` expands the row. The diff behind the write — the single thing they most wanted to verify — is unreachable without a mouse. low-vision-user hit the adjacent problem (M26): the click that does work is off by one row.

**Expected.** A roving focus over transcript rows (arrow keys or j/k) with a visible focus indicator, Enter/Space toggling the disclosure, so every diff and every block of tool output is keyboard-reachable.

**Repro.**
```
shell-use type "create a file named notes.txt, then show me the file"; shell-use press Enter
shell-use press Up; shell-use press Down; shell-use press Tab; shell-use press space; shell-use press Enter
# nothing selects, nothing expands, no indicator anywhere
shell-use mouse click --on-text "write_file notes.txt"   # only this works
```

**Screenshot.** `ux/keyboard-only-user/shots/03b-click.svg`

### B7 — A second Ctrl+C quits instantly with no warning (blocker, 2/10)

**What happened.** Ctrl+C is the only working interrupt, but the first press gives weak or no on-screen confirmation — developer-critic saw no `[interrupted]` marker at all and could not tell whether they had cancelled the turn or it had finished. cognitive-adhd, reading the same silence, pressed Ctrl+C again and the app died on the spot (`Input/output error (os error 5)` from the PTY thereafter). andrey-owner reproduced it: a second Ctrl+C while idle kills the process with no goodbye and no exit code. Ctrl+C is also the key that normally kills a CLI, so several personas hesitated before pressing it the first time, not knowing whether they were about to lose the session.

**Expected.** The first interrupt appends a visible `interrupted by user` boundary where the user is looking. An idle Ctrl+C requires confirmation or shows `press Ctrl+C again to exit`, and exits cleanly.

**Repro.**
```
shell-use type "write a very long essay about the history of version control, 100 lines"; shell-use press Enter
shell-use press Escape          # no effect
shell-use keys "Ctrl+c"         # interrupts; little/no visible confirmation
shell-use keys "Ctrl+c"         # app exits immediately
```

**Screenshot.** `ux/cognitive-adhd-user/shots/08b-ctrlc.svg` (also `ux/andrey-owner/shots/08b-ctrlc.svg`)

### B8 — `/quit` blanks the terminal, and once never exited (blocker, 2/10)

**What happened.** boomer-tech-averse ran `/quit` (described in `/help` as "tear the tree down and leave"), and the screen went entirely black — not one character, no farewell, no shell prompt — and sat there. non-native-english hit the harder case: `/quit` printed `leaving`, wiped the terminal to solid black, and the process was still alive 20+ seconds later; Ctrl+C returned `Input/output error (os error 5)` and it took SIGTERM to kill it. This is also the most likely explanation for M28 (developer-critic's lost history) — a shutdown path that does not complete would not flush the ledger.

**Expected.** `/quit` prints a one-line farewell, restores the terminal, and exits promptly; a shutdown that cannot complete shows a state and a force-quit key.

**Repro.**
```
shell-use type "/quit"; shell-use press Enter
shell-use text --full          # zero non-blank lines
ps -p <pid>                    # still alive
```

**Screenshot.** `ux/non-native-english/shots/09b-hung.svg` (also `ux/boomer-tech-averse/shots/09a-quit-black.svg`)

### M9 — Strip and rail paint on the transcript's baselines (major, 9/10)

**What happened.** Row 0 reads `○ sol                         idle- push_to_pr — Add commits to open PRs I've authored`; rows 1–2 do the same to the rail: ``  read mail `what can you do here…- bash — Any git operations via command line``. The SVGs confirm two independent text runs on the identical baseline (`y="51.26"`, one at x=15, one at x=315) — verified in both `developer-critic/shots/02b-done.svg` and `designer-critic/shots/02-answer.svg`. The rail column ends at col 34 and the transcript starts at col 34 with a zero-cell gutter, sometimes butting an ellipsis straight against the first body character, sometimes hard-cutting with no ellipsis (`intent (self-declared): 👋  Pong!Europe. This script featured t`). Personas read `idlePlease` and `running──` as words, and several said the first thing they would do is screenshot it as a corruption bug.

**Expected.** The strip owns its row and the transcript scrolls beneath it. The rail is separated from content by at least one blank column or a vertical rule, with hard clipping at the boundary.

**Repro.**
```
shell-use type "what can you do here?"; shell-use press Enter; shell-use wait idle
shell-use text --full | sed -n '1,3p'
```

**Screenshot.** `ux/designer-critic/shots/02-answer.svg`

### M10 — Streaming chunk boundaries baked in as hard line breaks (major, 10/10)

**What happened.** The renderer flushes on network chunk boundaries rather than word boundaries. Verbatim from the walks: `I'll create` / ` a file named notes.txt`; `Perfect` / `! I've created`; `Now` / ` let me show you the file:`; `- Ask questions back` / ` to you when I need decisions`. Worse, it breaks *inside words*: `This script featured lowercase letters with clear distin` / `ctions`, `- Update Linear ticket stat` / `uses`, `**Is this a short` / `hand reference**`. The same splits shred markdown across chunks — `**Code & File` / ` Operations:**` renders its asterisks literally, and one inline-code span opened on one chunk and closed three lines later, swallowing a whole paragraph into a code span. Three personas confirmed the breaks survive quit and relaunch, so they are persisted, not a paint artifact.

**Expected.** Accumulate the stream and wrap at word boundaries on paint; parse markdown over the accumulated document, not per chunk. The transcript should read identically to the finished text.

**Repro.**
```
shell-use type "explain how git rebase works in great detail, at least 60 lines"; shell-use press Enter
shell-use wait idle; shell-use text --full
# quit, relaunch same BOUGH_HOME, PageUp to the top — the same breaks are still there
```

**Screenshot.** `ux/designer-critic/shots/09a-relaunch.svg` (also `ux/non-native-english/shots/09d-top.svg`)

### M11 — Search results are raw ledger JSON and the hits are inert (major, 10/10)

**What happened.** Ctrl+F focuses the search field and the results are the event ledger, verbatim: `sol s54 request/header  {"as_of":53,"budget":96000,"call":{"effort":null,"max_tokens":8192,…`, `sol s8 thought/text  {"step_index":0,"text":"! I'm **sol**, an AI agent working within`. Most hits are `request/header` records that do not visibly contain the search term at all. There is no match highlighting, no match count, no arrow-key selection, and literal `\n\n##` escapes leak through at wide widths. Interaction is inconsistent across personas — click did nothing for five of them, jumped to the wrong region for three, and jumped correctly (but unhighlighted) for two — and for developer-critic and designer-critic the click permanently froze the transcript (see B2). Fuzzy matching is also unsignalled: searching `THREE` returned hits on "three lines" and "5-step mechanism".

**Expected.** Search over rendered conversation content. Result rows show `speaker · snippet` with the match highlighted, a `1 of N` count, arrow-key selection, and Enter/click scrolling the match into view with the hit highlighted and live-follow restorable.

**Repro.**
```
shell-use keys "Ctrl+f"
shell-use type "capabilities"
shell-use text | sed -n '25,31p'      # raw JSON rows
shell-use mouse click 50 25; shell-use press Down; shell-use press Enter
```

**Screenshot.** `ux/andrey-owner/shots/05c-searchterm.svg` (also `ux/designer-critic/shots/05b-search.svg`)

### M12 — Panels never dismiss and reserve ~10 rows when empty (major, 10/10)

**What happened.** After `/help`, the eight command lines pinned themselves to the bottom rows for the rest of the session — through every later turn, through Escape, through resizes, through other commands. The search box and its stale results do the same; one persona's `zz` query and another's `worker` results were still on screen at the end of the walk. The panel keeps its full height when nearly empty: `/agents` renders one row of content plus seven blank rows. When nothing has been run, the reserved pane is simply blank — rows 25–35 of 36, rows 14–22 of 24 at 80x24 (40% of the screen), ~9 rows even at 200x50. Panels also overpaint rather than replace: busy-executive ended up with transcript, ten stale JSON search rows, and the `/help` list stacked in the same region with no borders. The `/help` list renders flush at column 0 while everything else starts at column 34.

**Expected.** Escape dismisses the topmost overlay. Panels size to their content and collapse when empty; command output replaces rather than partially overpaints; the transcript expands into the freed space.

**Repro.**
```
shell-use type "/help"; shell-use press Enter
shell-use press Escape                 # panel persists
shell-use type "/agents"; shell-use press Enter   # 1 content row + 7 blank
shell-use resize 80 24; shell-use text | cat -n   # rows 14-22 blank
```

**Screenshot.** `ux/andrey-owner/shots/07a-80x24.svg` (also `ux/low-vision-user/shots/07a-80x24.svg`)

### M13 — Hardcoded 34-column rail; no re-wrap on resize; no max measure (major, 10/10)

**What happened.** The rail is exactly 34 columns at every terminal size. At 80x24 it takes 43% of the width to show two lines truncated at 30 characters (``read mail `explain in detail ho…``), leaving ~46 columns for the conversation — prose wrapping every four or five words. At 200x50 it is *still* 34 columns and *still* ellipsized, while the transcript runs to a 165-character measure with no upper bound and ~10 rows sit empty below. Resizing does not re-wrap already-rendered history: after `resize 200 50` old lines keep their 120-column wrap and the right 80 columns stay blank, frozen breaks and all.

**Expected.** A breakpoint that collapses or hides the rail below ~100 columns and lets it grow (or stop truncating) when there is room; history re-wraps on resize; prose measure capped at ~80–90 characters.

**Repro.**
```
shell-use resize 80 24;  shell-use text --full | cat -n
shell-use resize 200 50; shell-use text --full | cat -n
```

**Screenshot.** `ux/designer-critic/shots/07a-80x24.svg` (also `ux/boomer-tech-averse/shots/07b-200x50.svg`)

### M14 — Esc does not interrupt; the stop key is undiscoverable (major, 10/10)

**What happened.** With `● sol running`, Escape does nothing — no interrupt, and no feedback that the key was even received, so several personas pressed it three times before concluding it was dead. Ctrl+C works and (when the view is live) records `── wake end · interrupted` plus `cancelled`, which personas praised as honest. But nothing anywhere names it: the running strip carries no hint, and `/help` — which describes itself as "the commands and key hints this surface has" — lists no key bindings at all. Multiple personas said they pressed Ctrl+C only because they were willing to risk killing the process. Escape's only observable effect is silently deleting a non-empty composer draft, which keyboard-only-user hit while reaching for it as an overlay-dismiss reflex.

**Expected.** Escape interrupts the running turn (the convention in this class of tool). While running, the strip shows `esc to interrupt`. Escape on a draft either dismisses an overlay first or does not destroy the draft without confirmation.

**Repro.**
```
shell-use type "write a very long essay about the history of version control"; shell-use press Enter
shell-use press Escape; shell-use press Escape; shell-use press Escape   # status stays "running"
shell-use keys "Ctrl+c"                                                  # interrupts
```

**Screenshot.** `ux/designer-critic/shots/08a-esc.svg` (also `ux/keyboard-only-user/shots/08b-ctrlc.svg`)

### M15 — Config patch accept/reject is invisible in the app (major, 10/10)

**What happened.** Writing `$BOUGH_HOME/bough.patch.yml` produces no on-screen response of any kind — not for a valid patch, not for a schema-invalid one, not for a nonsense model name, not for deleting the file. The app is clearly watching, validating, and rejecting within seconds: `bough.log` carries `` WARN bough::watch: bough: patch rejected, last good tree still running: layer `user`: unknown field `model`, expected one of `entries`, `insert`, `remove` `` — a precise, well-written error the user never sees. Several personas then sent a message, got a normal reply, and could not tell whether their config had applied, been ignored, or been rejected. The fail-safe behaviour itself ("last good tree still running") was praised as correct engineering; only the surfacing is missing. Compounding it, the active model appears nowhere in the UI, so there is no way to verify what actually took effect.

**Expected.** Every patch reload renders in the app: `config reloaded` or `` patch rejected: unknown field `model` `` — the same text the log gets — plus the active model in the strip.

**Repro.**
```
printf 'model:\n  policy:\n    sol: totally-not-a-real-model\n' > "$BOUGH_HOME/bough.patch.yml"
sleep 5; shell-use text --full            # unchanged
tail -5 "$BOUGH_HOME/bough.log"           # the WARN is here
```

**Screenshot.** `ux/andrey-owner/shots/10b-patch-bad.svg`

### M16 — No onboarding on an empty home (major, 10/10; one persona rated it blocker)

**What happened.** First launch renders four pieces of text on 36 rows: `○ sol` and `idle` top-left, a dim unlabelled `search /` floating alone mid-screen, and `message, or / for a command` at the bottom — a sentence with no verb, which three personas re-read trying to parse (the SVG confirms the run is exactly that, so the leading "Type a" is genuinely absent). No product name, no version, no cwd, no model, no example, no help hint. `?` types a literal question mark. `F1`, `Ctrl+K`, `Ctrl+P`, `Ctrl+X`, `Ctrl+O`, `Ctrl+T`, `Ctrl+G` are all dead. Every persona found `/help` only by guessing it. boomer-tech-averse rated this a blocker: with no name, no folder, and no working help key, there was no way in at all.

**Expected.** A first-run strip naming the app, the cwd and the model, a visible `? help` hint, one example prompt, and a grammatical placeholder.

**Repro.**
```
rm -rf "$BOUGH_HOME" && mkdir -p "$BOUGH_HOME"
shell-use run --cols 120 --rows 36 -- bough
shell-use text --full | sed -n '1p;24p;36p'
shell-use type "?"    # types a character
```

**Screenshot.** `ux/designer-critic/shots/01-launch.svg`

### M17 — `/` opens no command palette although the placeholder promises one (major, 10/10)

**What happened.** The composer advertises `message, or / for a command`. Typing `/` at line start shows nothing; `/h`, `/he`, `/ag` show nothing; Tab completes nothing. The placeholder hint also disappears the moment a character is typed, so the only instruction on screen vanishes exactly when it is being followed. Every persona had to blind-guess `/help`. For keyboard-only-user this made the command namespace unusable: an unlisted namespace with no menu and no arrow-key navigation is not reachable.

**Expected.** Typing `/` at line start opens a filtering command menu, navigable with Up/Down, selectable with Enter or Tab.

**Repro.**
```
shell-use type "/"; sleep 2; shell-use text | sed -n 36p     # no menu
shell-use type "he"; shell-use text --full                    # still no menu
shell-use press Tab                                           # no completion
```

**Screenshot.** `ux/andrey-owner/shots/01c-slashhe.svg`

### M18 — `/help` lists no keys and speaks in internal metaphor (major, 10/10)

**What happened.** The entry reads `/help  the commands and key hints this surface has` and the output is eight slash commands with zero key bindings — so Ctrl+F, Ctrl+C, PageUp/PageDown and End, the only keys that do anything, are documented nowhere in the product. The descriptions assume the reader already knows the system: `/quit  tear the tree down and leave`, `/agents  the roster: status, trajectory, unconsumed mail`, `/drift  per-agent stability signals from the ledger`, `/oldfeed  what the old-feed bridge last swept`, `/reconsolidate  distil, surface contradictions and expire stale evidence`. non-native-english understood every word separately and could not predict what any command would do; boomer-tech-averse understood one line of eight. The typed `/help` is also never echoed into the transcript, so there is no record of what produced the output.

**Expected.** Plain literal descriptions (`/agents — list running agents and their status`), the actual key bindings listed, and the command echoed with its output.

**Repro.**
```
shell-use type "/help"; shell-use press Enter; shell-use text --full | sed -n '28,35p'
```

**Screenshot.** `ux/cognitive-adhd-user/shots/01e-help.svg` (also `ux/non-native-english/shots/01e-help.svg`)

### M19 — Markdown is half-rendered (major, 8/10)

**What happened.** Headings never render: `## Core Capabilities`, `## Limitations`, `### 📝  Code & File Management`, `## Key Topics Explained:` all show their hashes. Bold is inconsistent — `Task Management:` rendered bold in one answer while `**sol**` and `- **Work with branches**` rendered literally in the next. Tables print as raw pipes (`| Scenario | Use |` / `|----------|-----|`). Inline code splits across lines leaving orphan backticks (``- `open_pr`` / `` ` — Open pull requests ``). Some of this is M10's chunk-boundary damage, but headings fail even in complete, restored text, so the two are separable. Personas without a markdown mental model (boomer, non-native) read the punctuation as the program being broken.

**Expected.** Render markdown or strip the markers — consistently, over the accumulated document.

**Repro.**
```
shell-use type "what can you do here?"; shell-use press Enter; shell-use wait idle
shell-use press PageUp    # ×5, read the heading lines
```

**Screenshot.** `ux/low-vision-user/shots/09b-top.svg` (also `ux/boomer-tech-averse/shots/07b-200x50.svg`)

### M20 — Readline gaps (major, 9/10)

**What happened.** Ctrl+U deletes a single character — `abcdefgh` → `abcdefg` — in 8 of 10 walks (cognitive-adhd reported it clearing correctly, so it may be state-dependent). Two personas saw it behave as an *undo of a kill* instead: on an empty composer it pasted back previously cleared text, which busy-executive read as the app resurrecting an old prompt on its own. andrey-owner traced a real cost: a Ctrl+U that left a stray `z` turned `/agents` into `z/agents`, which was sent to the model and burned a whole turn on "you've sent me 'z/agents' — this looks like it might be a reference or identifier…". Up-arrow recalls no sent-message history in any walk — the single most-used key in a REPL. Tab and Ctrl+L are dead everywhere. Shift+Enter is inconsistent: a newline for keyboard-only-user, nothing at all for andrey-owner, leaving no reliable keyboard way to add a line. Ctrl+A, Ctrl+E, Ctrl+K and Ctrl+W behave correctly, which makes the gaps more jarring, not less — two thirds of the set works, so muscle memory assumes the rest does.

**Expected.** Ctrl+U kills to line start; Up/Down cycle sent-message history; Tab completes commands and paths; Shift+Enter (and Alt+Enter) insert a newline everywhere.

**Repro.**
```
shell-use type "abcdefgh"; shell-use keys "Ctrl+u"; shell-use text | sed -n 36p   # "abcdefg"
shell-use press Escape; shell-use keys "Ctrl+u"                                    # old text returns
shell-use press Up                                                                 # no history
shell-use type "line one"; shell-use keys "Shift+Enter"                            # no newline
```

**Screenshot.** `ux/power-user/shots/11h-wedged.svg` (also `ux/andrey-owner/shots/11g-shiftenter.svg`)

### M21 — No selection or copy, and the mouse grab kills native selection (major, 9/10)

**What happened.** Dragging across a line of an answer produces nothing: no highlight, no toast, no clipboard change. The SVGs confirm there is no selection rendering at all — the only rects in the captures are the page background and the cursor cell (one persona saw a single stray 10px cell). Because the app enables mouse reporting, the terminal's own selection is also disabled, and the alt-screen has taken away scrollback, so there is no way at all to get text out of the window — and nothing hints that Shift/Option-drag might restore native selection. low-vision-user copies text out to read it in a magnifier and could not; busy-executive noted that getting a command out of an answer is a daily need.

**Expected.** Either in-app drag-select with a visible highlight and a brief "copied" flash (bough has a copy-flash overlay elsewhere), or release the mouse grab on drag / advertise the Shift-drag escape hatch.

**Repro.**
```
shell-use mouse drag 34 10 110 10
shell-use screenshot -o /tmp/drag.svg    # no highlight rect, no toast
```

**Screenshot.** `ux/designer-critic/shots/04e-dragselect.svg` (also `ux/low-vision-user/shots/04d-drag.svg`)

### M22 — Contrast hierarchy is inverted (major, 8/10)

**What happened.** Measured from the SVGs against the `#282d35` background: body prose `#d0d0d0` ≈ 8.9:1 (good), accents blue `#7aa2f7` 5.5:1, purple 5.98:1, green 7.57:1 (all pass). But `/help` and **every error message** — `` unknown command `nonsense` ``, `` unknown command `tmp` ``, `cancelled`, `leaving` — are `#565f89` ≈ 2.1–2.4:1, the most recessive colour in the palette. Secondary chrome is `#6f7783` / `#707680` ≈ 3.0:1 and carries load-bearing information: the filename in every tool row, the diff file header `── notes.txt`, the hunk header, the turn markers `── wake` / `── wake end · completed`, the rail lines, `search /`, and the composer placeholder. So the instructions and the failure reports are the least legible text in the product, and the identifiers a low-vision user most needs are dimmed as if decorative. Personas read `search /` as leftover screen debris for four steps and nearly ignored `/help` as disabled.

**Expected.** No text below 4.5:1. Errors in a warning/red hue with an icon, visually distinct from inert help text. Filenames and turn markers are primary information, not chrome. Offer a high-contrast theme.

**Repro.**
```
shell-use type "/help"; shell-use press Enter
shell-use type "/nonsense"; shell-use press Enter
shell-use screenshot -o /tmp/c.svg; grep -o 'fill="#565f89"' /tmp/c.svg | wc -l
```

**Screenshot.** `ux/designer-critic/shots/06a-help.svg` (also `ux/low-vision-user/shots/03e-diff.svg` for the #707680 filenames)

### M23 — Mouse wheel does not scroll the transcript (major, 7/10, contradicted by 1)

**What happened.** Seven personas found the wheel completely inert over the transcript, verified by byte-identical `shell-use text` before and after — while idle, while streaming, and while parked mid-history, at amounts from 5 to 200 notches. The app clearly receives mouse events, since click-to-expand works. designer-critic reported the exact opposite: keyboard scroll dead, wheel the only working mechanism. That contradiction is the strongest evidence that scrolling is gated on the same invisible focus state as B1/B2 rather than being a wheel bug per se. For boomer-tech-averse, whose only scroll instinct is the wheel, this alone removed access to the conversation.

**Expected.** The wheel scrolls the transcript viewport unconditionally, regardless of focus.

**Repro.**
```
shell-use mouse move 80 15
shell-use text --full > /tmp/a.txt
shell-use mouse scroll up --amount 8
shell-use text --full > /tmp/b.txt; diff /tmp/a.txt /tmp/b.txt   # identical
```

**Screenshot.** `ux/cognitive-adhd-user/shots/04c-scrolled.svg`

### M24 — The status strip carries no model, cost, context or cwd (major, 4/10)

**What happened.** On a 120-column screen the entire strip is `○ sol                         idle` — roughly 100 blank columns. Nothing names the model that is answering, tokens or dollars spent, context remaining, or the directory the agent is writing into, while the agent creates files there (`notes.txt`, `git_rebase_explanation.md`). Combined with B5, a user has no way to notice that writes are landing somewhere else, and combined with M15, no way to see which model a config change actually selected. bough's own main line does all of this today.

**Expected.** Model name, cwd, %-context-left and cost in the strip, plus the interrupt key while running.

**Repro.**
```
shell-use run --cols 120 --rows 36 -- bough
shell-use text | sed -n 1p     # "○ sol                         idle"
```

**Screenshot.** `ux/andrey-owner/shots/01-launch.svg`

### M25 — The agent advertises tools it does not have (major, 4/10)

**What happened.** Asked "what can you do here?", the first answer every persona sees lists `open_pr`, `push_to_pr`, `linear_write`, `bot_thread_op`, `spawn_worker`, `ask` — "Open pull requests as Andrey", "Update Linear tickets (change status, add comments)", "Interact with GitHub/bot review threads" — alongside the real `write_file`, `read_file`, `bash`. Nothing in `/help` or the UI corroborates a Linear or GitHub integration, and no persona saw one used. It also speaks in internal vocabulary ("a worker in the lane/sol trajectory", "read mail", "the task tree"), which boomer-tech-averse read as having accidentally sent an email. developer-critic connected it directly to B5: an intro that reads as confident fiction, from a tool that had just written into the wrong repo, is what makes a user stop trusting the checkmarks.

**Expected.** The capability answer is grounded in the tools actually registered for the session, in the user's vocabulary.

**Repro.**
```
rm -rf "$BOUGH_HOME" && mkdir -p "$BOUGH_HOME"; shell-use run -- bough
shell-use type "what can you do here?"; shell-use press Enter; shell-use wait idle
```

**Screenshot.** `ux/busy-executive/shots/02-answer.svg`

### M26 — Click hit-test is off by one row (major, 1/10)

**What happened.** With `▸ write_file notes.txt` on row 12 and `▸ read_file notes.txt` on row 13, clicking row 12 expanded `read_file`; clicking row 11 expanded `write_file`. Clicking directly on an expanded `▾` row — on the glyph itself, at columns 34, 35 and 40 — never collapsed it. Only low-vision-user tested this systematically, and for them the compounding cost is real: clicking is already effortful, and being one row off makes every click a guess. It also interacts with B1 — a mis-aimed click still steals focus.

**Expected.** Clicking a row toggles that row; clicking an expanded row collapses it.

**Repro.**
```
shell-use type "create notes.txt with three lines, then read it back"; shell-use press Enter; shell-use wait idle
shell-use text | cat -n | sed -n '11,14p'     # note the row numbers
shell-use mouse click 50 11                    # expands the row below the one clicked
shell-use mouse click 35 7                     # on a ▾ glyph — nothing collapses
```

**Screenshot.** `ux/low-vision-user/shots/03e-diff.svg`

### M27 — Four of eight documented commands are silent no-ops (major, 1/10)

**What happened.** power-user ran every command `/help` lists. `/agents`, `/help` and `/quit` do something. `/focus sol`, `/drift`, `/drift sol`, `/oldfeed` and `/prime unix` produced zero visible change anywhere — no panel, no error, no log line in the UI — indistinguishable from the app ignoring the keystroke. `bough.log` had the answer for at least one of them (the old-feed bridge is disabled with no `jungler.db`), which the UI never said. A documented command that silently does nothing is worse than an undocumented one.

**Expected.** Every documented command renders output or states why it cannot run (`/oldfeed: bridge disabled — no jungler.db`).

**Repro.**
```
for c in "/focus sol" "/drift" "/oldfeed" "/prime unix"; do
  shell-use type "$c"; shell-use press Enter; shell-use text --full; done
grep -i 'oldfeed\|bridge' "$BOUGH_HOME/bough.log"
```

**Screenshot.** `ux/power-user/shots/11f-cmds.svg`

### M28 — History not restored on relaunch (major, 1/10 — contradicted by 8)

**What happened.** developer-critic ran `/quit`, relaunched with an identical `BOUGH_HOME`, and got a completely empty transcript — no messages, no session picker, no resume hint. Ctrl+F for `rebase`, a word from an answer minutes earlier, returned zero hits, so it was not reachable even by search. `ledger.db` was 4.1k with a 231k WAL, so the data was on disk and unmerged. Eight other personas listed flawless history restore as a *delight*, including one whose app had been hard-killed mid-turn. The most likely explanation is the B8 shutdown path: a `/quit` that blanks the screen and does not complete would leave the WAL uncheckpointed. Treat as an intermittent shutdown-flush bug, not a general persistence failure.

**Expected.** Relaunching into the same home restores the prior conversation, or offers to resume, or at minimum leaves it searchable.

**Repro.**
```
# after several turns:
shell-use type "/quit"; shell-use press Enter
ls -la "$BOUGH_HOME"/ledger.db*          # note WAL size
shell-use run -- bough                    # same BOUGH_HOME
shell-use text --full                     # empty
shell-use keys "Ctrl+f"; shell-use type "rebase"    # zero hits
```

**Screenshot.** `ux/developer-critic/shots/09c-relaunch.svg`

## 4. Delights (deduped)

Praised by name across the walks — protect these in any refactor.

1. **Tool-row disclosure is the best-designed component in the app** (10/10). `▸ write_file notes.txt … ✓` collapses to one clean line with a right-aligned status glyph; one click swaps `▸`→`▾` and reveals the detail; one click collapses. Instant, no flicker. designer-critic: "correct affordance, correct information density."
2. **The write_file expansion is a real unified diff** (6/10): a `── notes.txt` file rule, an `@@ -1,0 +1,3 @@` hunk header, and `+` markers. Correct mental model; personas trusted it immediately.
3. **Status is never colour-only** (low-vision-user, explicitly): the strip pairs a glyph with a word (`○ idle` / `● running`), tool rows pair `▸`/`▾` with `✓`, diff lines carry `+` as well as green.
4. **Message queueing is better than most chat UIs** (8/10). Two messages fired back-to-back while a turn was running were both echoed, batched into one wake, and answered in order. No drops, no double-sends, no queue corruption.
5. **History survives quit, relaunch, and even a hard kill** (8/10) — all the way back to the first message, with the interrupted turn honestly marked rather than pretending it finished. (One counter-report: M28.)
6. **Ctrl+C is a genuinely safe stop** (9/10): it cancels the turn without killing the app, keeps the partial answer, prints `cancelled`, and records `── wake end · interrupted`. Personas called this honest.
7. **Bracketed paste is handled properly** (6/10): the composer grows upward, keeps line structure intact, and does *not* auto-submit. (The raw-bytes path is B4.)
8. **The config watcher fails safe**: a malformed `bough.patch.yml` is rejected with "last good tree still running" rather than crashing or half-applying. The engineering instinct is right; only the surfacing is missing (M15).
9. **Resizing mid-stream never corrupts or crashes** (7/10) — 120x36 → 100x30 → 140x40 while a list was streaming, no tearing, no lost content.
10. **Turn boundaries give the transcript a rhythm**: `── wake` / `── wake end · completed` makes it scannable once the vocabulary is known (the vocabulary itself is nit 37).
11. **The palette is good** — body 8.9:1, blue 5.5:1, purple 5.98:1, green 7.57:1, all clearing AA. designer-critic: "the colour *choices* are good; they're just applied to the wrong elements."
12. **`` unknown command `nonsense` `` is a clean, honest error** that quotes exactly what was typed (its problems are placement, contrast and text loss, not wording).
13. **Empty Enter is a no-op** rather than sending a blank turn (4/10).
14. **Ctrl+A / Ctrl+E / Ctrl+K / Ctrl+W behave like readline** (4/10).
15. **The agent does the work it claims**: `notes.txt` was genuinely on disk with exactly the three requested lines, 129 bytes, verified by six personas.
16. **Clicking a search hit really does jump the transcript** to that moment — the navigation mechanism works; only the rendering and the return path are broken.

## 5. Conventions checklist (Claude Code / Codex expectations)

| Convention | Expected by | bough today | Verdict |
|---|---|---|---|
| **Esc interrupts a running turn** | 10/10 personas tried it first | No effect at all, and no feedback that the key was received; Ctrl+C is the only interrupt and is documented nowhere | ❌ M14 |
| **Enter sends** | all | Works — except when focus has silently moved (B1), when the message starts with `/` (B3), or when a raw paste splits it (B4) | ⚠️ works, but three silent-loss paths |
| **Shift+Enter inserts a newline** | 2 tested | Contradictory: a newline for keyboard-only-user, dead for andrey-owner; no reliable keyboard newline exists | ⚠️ M20 |
| **`/` opens slash autocomplete** | 10/10 (the placeholder promises it) | Nothing on `/`, `/h`, `/ag`; Tab completes nothing; commands must be typed blind | ❌ M17 |
| **`@` mentions for files** | not attempted by any persona | No `@` picker observed anywhere; not advertised in the placeholder or `/help` | ❓ absent, untested |
| **Status bar with model / cost / context / cwd** | 4 called it out; 10 saw the empty strip | `○ sol   idle` and ~100 blank columns; no model, cost, context or cwd anywhere in the UI | ❌ M24 |
| **Visible thinking / tool state** | 10 | Tool rows with `▸`/`▾` and `✓` are excellent (delight 1); but the only progress signal is one word changing colour in the corner — no spinner, elapsed time, step counter, or `esc to interrupt` hint | ⚠️ tools ✅, progress ❌ (M32) |
| **Copy feedback** | 9 | No selection rendering, no copy, no toast — and the mouse grab disables the terminal's own selection with no Shift-drag hint | ❌ M21 |
| **Resize safety** | 8 | No crashes, no tearing, no lost content mid-stream (delight 9) — but history never re-wraps, the rail stays 34 columns at 80 *and* 200, and one persona saw spurious blank lines injected | ⚠️ safe, not responsive (M13, nit 39) |
| **Scrollback: PageUp/PageDown/Home/End + wheel + jump-to-latest** | 10 | Focus-dependent and mutually exclusive; no End, no scrollbar, no position indicator, no unread marker | ❌ B2 |
| **Escape dismisses the topmost overlay** | 5 | Dismisses nothing — not `/help`, not search; its only observable effect is deleting a non-empty draft | ❌ M12 |
| **`?` or `/help` documents key bindings** | 10 | `?` types a character; `/help` advertises "key hints" and lists none | ❌ M18 |
| **A focus indicator exists** | 10 | Nothing on screen ever indicates which region has the keyboard, in any state | ❌ B1 |
