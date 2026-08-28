# Phase ux-visual — the frame, in two passes

Subject: the visual audit of 2026-08-27 (fifteen findings, no blockers; the screenshots are in the
audit artifact and its driver is `scripts/ux/`-shaped but lives outside the repo). The persona
audits judged whether a person could get through; this one judged what the frame TELLS the eye.
Two passes, because the codemode branch edits `tui-focus`'s row model at the same time:

- **Pass A (this branch, first):** tui-shell, tui-strip, tui-status, tui-render, tui-search's
  registration, the `/agents` and `/drift` outputs.
- **Pass B (after the codemode merge):** tui-focus rows — speaker labels, one turn rule instead
  of three lines, machinery rows hidden, ✓ beside the tool, click-any-row — and the search
  index's about-line echoes.

## Decisions (D-uxv-1 …)

**D-uxv-1 — an `Aux` pane costs rows only while it has something to show.** The search pane
took twelve rows at every launch; at 200×50 that was half the frame empty (F1). The Pane trait is
frozen for the other tracks, so the mechanism is additive: `RenderCx::report_aux_rows(n)` joins
`report_rows`, `RowReport` carries `aux_rows: Option<u16>`, and `pane::layout_with` gives an
`Aux` pane the rows it reported last frame — except the FOCUSED pane, which always gets its
registered size, which is how Ctrl+F opens a collapsed pane. A pane that never reports is laid out
exactly as before (track C's panes). The shell carries a report forward for a pane that got no
rows, or a collapsed pane would forget it asked for zero and spring back. Esc on an empty search
falls through to the shell, which already returns the keyboard to the composer.

**D-uxv-2 — the notice band and the palette end where the Status band begins.** They borrowed
"every row above the composer", and the status line is the row above the composer (F4). A
persistent notice (`/help`, a command's output) scrolls with PgUp/PgDn; the truncation markers name
the key (`… 11 more lines (PgDn)`), and `notice_scroll_max` keeps the last page full.

**D-uxv-3 — three new theme roles, and the dim raised.** `interactive` (tool headers, claim
buttons: things you click), `code` on `code_bg` (inline code and code blocks: a texture, not a
colour), `field_bg` (the composer's band). Headings are weight and a rule, not the accent, so the
accent means one thing: who is speaking. `dim` moves from `#8b92a1` to `#9aa2b1` — 4.3:1 on a
`#282d35` terminal was under AA (F6). The contrast audit measures `code` against `code_bg`.

**D-uxv-4 — dormant is `warn`, never `dim`.** The state that decides whether mail is answered
was the faintest thing on the rail (F6). `/agents` reads the same `agent/dormancy` step the rail
does, by name, with no dependency on the `dormancy` row (P3-D11), so the two surfaces agree (F13).

**D-uxv-5 — the rail says who leads and who has the keyboard.** The leader is the one cross-lane
agent and nothing on screen said which (Andrey's ask). `tui-strip` injects the `leader` key
OPTIONALLY: absent at activation means no tag for this life of the row; the leader row reloading
against another agent rebinds the key and reloads the rail with it, which is exactly when the tag
should move. The leader sorts first and wears ` ✦ leader`; the focused lane carries the
transcript's own `▌` row-focus glyph in column 0, so one mark means "the keyboard's conversation"
in both panes. The intent half's 23-column label becomes `→ ` plus italics in the `thought`
colour (F8); the full words stay in `/help`.

**D-uxv-6 — a status chip with no value is absent.** `— · — ctx · —` before the first turn said
nothing three times (F11). `ctx` reads `99% ctx left`. Nothing is ever invented as a zero.

**D-uxv-7 — `/drift` is sentences first, numbers after.** The whole stats line survives as
`raw: …` at the end (F14).

**D-uxv-8 — markdown rhythm.** Consecutive list items are one block (no blank between); a code
block is padded to the measure on `code_bg`; h1/h2 are bold + underlined in the body colour (F10).

## Verification

Per-crate: `make gate-crate CRATES="bough-plugin-tui-shell bough-plugin-tui-strip
bough-plugin-tui-render bough-plugin-tui-search bough-plugin-tui-status bough-plugin-drift-watch
bough-plugin-tui-focus"` — the tests that pinned the old behaviour were updated with the reason
in each (`notice_tests`, `rail::tests`, `wrap.rs`, `status.rs`, `drift-watch::command`). On
screen: the audit driver re-run against the release binary — the idle search slot is gone, the
status line survives `/help` and `/sleep`, the leader tag and focus marker render, dormant is
amber, `/agents` and the rail agree. Full: `make gates`.

## Not in pass A

Everything in tui-focus (F2 speaker labels, F3 machinery rows, F7 ✓ placement, F15 welcome
block, click-any-row) and the search index (F12). Pass B lands them once the codemode merge has
settled `rows.rs` / `expand.rs` / `program.rs`.
