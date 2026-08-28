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

## Verification

Per crate: `make gate-crate CRATES="bough-plugin-tui-focus bough-plugin-tui-strip
bough-plugin-tui-shell"`. On screen: `bough-next` (code mode, live haiku) — the program line
names its calls, the opened block is one ground, the rail wraps and carries the clock, no rule
after a completed turn, no marker after a click; `27-drafts.sh` asserts the card; `16`, `20`,
`23`, `12` re-run for the layout without the drafts pane.
