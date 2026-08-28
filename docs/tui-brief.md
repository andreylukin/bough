# The TUI brief — 2026-08-28

Subject: what Andrey said he wants from the TUI, asked with the grill-me method (rounds of
questions until nothing is silently assumed) against the merged rebuild in code mode, and what
each answer became on screen. The before/after mockups are in the brief artifact; this is the
ledger of what landed. Branch `ux-brief`, on top of `ux-pass-b`.

## What he said

Solo coding and directing a small fleet, about equally. **Mouse-heavy.** A click acts and
nothing else. The collapsed program line names its calls; opened, it shows everything. Tight,
like a log. The rail is always there and says what each lane is doing, in full. Drafts are cards
in the conversation. Command output stays the pinned band. Four status chips, no more. A visible
send/stop and a lane switcher on the composer.

## Decisions

**D1 — a click acts; it does not mark or move the keyboard.** `tui-focus`'s Click arm no longer
places the row marker; the marker is the keyboard's row (Up/Down after Tab). The click-any-row
mapping (`RowFocus::row_at_line`, `FocusState::area_y`) is gone. The bare-click
selection-clearing on mouse-up (D-uxv-20's other half) stands.

**D2 — the collapsed program line names its calls.** `program::calls_gist`: `view main.rs, view
README.md · 1ms`, each call as `name object` where the object is the argument a person would
name it by (`path`, `cmd`, `pattern`, …, else the first string), clipped at 32 columns. Calls
past the budget become `+N`; the bare count (`summary`) only when not even the first fits.

**D3 — an opened program sits on the code ground.** `program_lines` paints every line after the
header on `code_bg`, padded to the width — a line style paints only under its text, and a ground
that stops at each line's end is stripes, not a block.

**D4 — the rail says what each lane is doing, in full.** The about halves WRAP to up to
`about_lines` (now 3) lines each at the rail's width (`rail::about_halves`); past the cap the last
line is elided so a cut is visible. The intent mark sits on the first line, continuations indent
under it. The state word carries a clock (`running 2m`, `idle 41s`, `1h05`): `RailRow.since` is
stamped when the status CHANGES (a re-announcement does not reset it), and the pane stamps
`clock_text` per frame from the view's `now`, so the renderer stays pure.

**D5 — the turn rule only when the ending is news.** `wake/end` with `completed` (or no reason)
produces no row: the next speaker label says the turn ended. `interrupted`, `aborted`, crash
repair keep `── turn …`.

**D6 — a draft is a card in the transcript.** `Row::Draft` from `draft/message` and
`draft/ticket` BY NAME; `draft::card` renders `✎ draft · ticket  to: linear  <subject>  not sent`,
the body folded past four lines, and two buttons: `copy` (the draft as it would be sent, headers
first) and `open`/`close` (the whole body in place, through the same `Expanded` set tool calls
use). **No send**: nothing an agent drafts leaves this machine by itself — track B's rule stands,
now on the card. `tui.drafts` leaves the bundle; the crate stays for its own tests.
*Open until seen:* a `send` button needs an action provider that can carry a draft to its
audience; none exists yet.

**D7 — send/stop and a lane switcher on the composer.** `composer::chips` lays out `to: sol ▾`
and `send ⏎` (or `stop ⎋` while a turn runs) at the right edge of the band, on the selection
ground; the same function decides the hit test. `send` is Enter's path (a `/` line dispatches);
`stop` is Esc's; `to:` moves the focus to the next lane by name — the rail's click is the direct
pick. Bands under 60 columns get no chips. `ShellView.running` carries the running flag.

**D8, D9, D10 — kept.** The pinned command band; `cwd · model · ctx left · cost`; search, palette,
help, welcome.

## Rounds 5 and 6 — after three persona tests (power-user, developer-critic, cognitive-adhd)

**R5 — the `to:` chip is a picker.** `composer::lane_picker`: a short list hanging off the chip's
right edge on the rows above the band; click a row or Up/Down + Enter focuses that lane, Esc or a
click elsewhere closes it. (Cycling was five clicks with five lanes, and `▾` promised a list.)

**R5 — the rail caps each about half at two lines.** Six lanes at three each was most of a frame.

**R5 — a program over budget groups its calls by verb.** `program::verb_groups`: `3 views, 2
edits, bash`; names when they fit, the bare count only when not even the groups do.

**R5 — a live line while a call runs.** `rows::running_line`: `▸ running · bash cargo test · 12s`
at the bottom of the agent's span, from the call step's time and the view's now.

**R6 — the about-line is the work.** `about-line/compose.rs`: the state half is the tools it ran
on what (`viewed main.rs; patched README.md; ran \`cargo test\``), typed and program calls alike,
falling back to the first line it said; the intent half is the FIRST line of its last message.
The mail it was woken by is not a clause and not cited — `read mail <my own prompt>` on the rail
read as a bug to all three personas, and the reply's last line read as a stale, backwards intent.

**R6 — `✎ changed a.rs · b.md` at each turn's end.** `rows::changed_files` from every
patch/edit_file/write_file call (typed rows and program sub-calls); the line lands after the
turn's last agent row, in the added colour, once nothing is in flight. "What did it do to my
files" was invisible among the `▸ program …` lines.

**R6/R8 — `andrey: · queued`.** `Row::Queued` is drawn from the `inbox/spliced` insert that
queued a message from Andrey and dropped when a later splice claims or removes it (its
`mail/delivered` then draws it as his). Live, a message sent while a turn runs is spliced into
the RUNNING turn (`next_step`) and delivered at its next model step, so it appears as a plain
`andrey:` row mid-turn — the tag shows only for a message that genuinely waits for a next wake.

**R6 — Tab reaches the conversation first; the rail takes Up/Down.** `cycle_focus` puts Strip
panes last; in the rail `rail::step_focus` moves the focused lane and its head line sits on the
selection ground while the rail has the keyboard.

**R7 — names and noise.** The middle pane is "conversation" everywhere (its `PaneSpec` title;
`/help` and the branch picker follow). `/agents` drops the `waiting` column — the rail's `✉ N` is
the one place for unread mail. A code-mode file handle (`[README.md#B749]`) reads as its path in
the program line, the `✎ changed` line and the rail (`rows::unhandle`). The outer code-mode `run`
call is the program's envelope and says nothing on the rail; its inner calls are the clauses.
Andrey chose no click hint on `▸` rows: the glyph is enough.

**R7b/R8 — from the second persona round.** The intent's first line skips leading punctuation
and list/fence markers (`. Added the comment` read as a leaked fragment); a code-mode handle at
the head of ANY argument reads as its path (`tui-render::unhandle_head`), so an opened program's
inner `▸ patch …` rows match its header. A talk-only turn's rail state is `replied: <first line>`
— labelled, so `6` never reads as work. Scrolled up with nothing new, the conversation shows
`↑ older · End for newest` (`Viewport::badge`), becoming `↓ N new · End` when rows arrive.
Failed attempts before the call that finally succeeded fold under `▸ N failed attempts · open`
(`rows::retry_folds`), narration included; a failure that never succeeded stays inline, and the
success must be the SAME tool — a failed `read_file` before a successful `write_file` is two
things, not a retry (02-tool-calls caught the first draft folding them). A call-less program
that printed says `printed "…"`, not `0 calls`.

**R8b — from the second power-user pass.** Esc closes the `to:` picker before the keymap's
dismiss runs; a click on the chip while the list is open closes it (a toggle, not a reopen);
Tab skips a pane with no rows on screen (the collapsed search pane was a dead stop, and it read
as "the rail's keys do nothing"); the wheel over the rail scrolls the conversation. The about-
line reads the wake's `thought/text` flushes as MESSAGES (consecutive flushes of one model step
joined raw, as the conversation joins them), so `replied: I` for a reply that began `I` / `'ve …`
cannot happen. Two persona findings were driver artefacts, kept here so nobody chases them:
`mouse click --on-text "terra"` hits the rail's row, not the picker's; a wheel with no
coordinates lands on the rail.

**R9.** A program with no inner call, nothing printed and no error draws nothing
(`rows::is_empty_program`): code mode runs one for a plain reply, and `▸ program 0 calls ✓` on
every chat-only turn was noise. A talk-only turn has no `→` intent line on the rail: it would
repeat the reply the `replied: …` state already carries.

**R10 — from the busy-executive pass.** With the rail collapsed the status chip carries the
state (`sol · running`), not just the name. A failed call says why on its own line
(`▸ draft_ticket … ✗ tags isn't a valid parameter`), clipped to half the width. A "what do I
owe" chip: `◇ 1 claim · ? question` on the status line (`rows::owed` from the transcript's rows,
reported each frame; dropped last but for the stop key and the product) and `◇N` / `?` after the
lane's name on the rail (`rail::open_claims`, `rail::pending_question` from the ledger by name,
re-read when a wake ends). The question rule is a heuristic, no model call: the last message
ends with `?`, the turn is over, nothing from Andrey followed. A lane with no work yet is dim;
the leader never is. Not done: clicking the chip to jump to the claim/question.

**R10b — from the keyboard-only pass.** With the keyboard in the rail, every rail line wears
the `▎` ring the conversation wears (the rail had no visible sign of holding the keys). Esc
closes a search that has anything on screen from anywhere — after Enter on a hit the keyboard
had left it, and Esc in the composer left the hits up with no way out but Ctrl+F. Enter on a hit
moves the row marker to the hit (`retarget` sets `RowFocus::on_step`). The row marker is drawn
only while the conversation has the keyboard (`FocusState::keyboard_here`), so nothing claims
the keys are somewhere they are not. `term.rs` pushes the keyboard-enhancement flags
(`DISAMBIGUATE_ESCAPE_CODES`) on terminals that support them and pops them on the way out —
without them Shift+Enter arrived as `ESC [27;2;13~` and fused two lines into one.

**Borders (Andrey, 2026-08-28).** Heavy rules between the panels: `┃` down the gutter between the
rail and the conversation, `━` across the row under the conversation, `┻` where they meet, in the
dim colour. `TuiConfig.borders` (default true); `layout_with` takes the rule row from the
conversation so no band moves. Off with `borders: false` on the `tui` row. The painter checks
every cell against the BUFFER's area, not the frame size: on a resize the two differ for a frame,
and an index past the buffer is a panic that took the process down at 80×24 (and froze the frame
after a swap in 36-swap-digging) before the guard. Scripts that read the conversation with
`cut -c35-` now see the gutter rule `┃` as their first character.

**Fixed along the way:** the `search [▏]` ghost after Esc, Esc (the layout's focused pane is the
KEYBOARD's pane, `run::layout_focus`); the welcome hint says `? for help`, the status line's
word, and says `tab panes` once.

**Open:** a `send` button on draft cards (no action provider can carry a draft); `draft_*` is
unreachable under tools-codemode (bough-d5's parity gate); the pane is still called "trajectory"
in `/help` and "conversation" in the key hints.

## Verification

Per crate: `make gate-crate CRATES="bough-plugin-tui-focus bough-plugin-tui-strip
bough-plugin-tui-shell"`. On screen: `bough-next` (code mode, live haiku) — the program line
names its calls, the opened block is one ground, the rail wraps and carries the clock, no rule
after a completed turn, no marker after a click; `27-drafts.sh` asserts the card; `16`, `20`,
`23`, `12` re-run for the layout without the drafts pane.
