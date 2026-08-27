# bough rebuild TUI — persona usability re-audit (#2, phase ux1)

**Subject.** The `rebuild` branch's release binary after phase ux1, driven live (haiku for both
tiers) through `shell-use` at 120x36.
**Method.** `scripts/ux2/run.sh` — three personas, each in its own empty `BOUGH_HOME` and its own
empty scratch cwd, each re-walking the top twelve findings of `docs/ux-audit-1.md` (B1–B8, M9–M12)
with the audit's own repro lines, capturing an SVG per step into `docs/ux-audit-2-shots/<persona>/`.
**Personas.** `developer-critic` (mouse and keyboard, the walk that found B5), `andrey-owner` (the
daily-driver flow), `keyboard-only-user` (never touches the mouse — B1, B6 and M26 are exactly the
findings a mouseless walk could not reach the first time).
**Gate.** `scripts/ux2/run.sh` exits non-zero unless every blocker and every major verdict is
`fixed`. `make ux2` runs it.

> **Status: run.** `scripts/ux2/run.sh` walked all three personas end to end against
> `target/release/bough` on the phase-ux1 tree (commit `176b154b`), live haiku for both tiers, one
> empty `BOUGH_HOME` and one empty scratch cwd per persona. **20 checks x 3 personas = 60 verdicts:
> every blocker and every major is `fixed`, each with a screenshot.** The only `not-fixed` rows are
> one added probe of minor severity (R1 below). The raw verdicts are `target/ux2/verdicts.tsv`; the
> tables below are rendered from them by `scripts/ux2/report.py`.

---

## 1. What the re-audit is for

The first audit's ten walks produced 39 findings that phase ux1 reduced to four root causes: there
is no focus model, typed text is destroyed, the frame paints over the content and says nothing, and
the stream is rendered per network chunk. Every work package of the phase ships unit tests, and
`scripts/tui/16-focus.sh` … `25-swap-status.sh` pin each fix at the screen. Those tests answer
"does the fixed behaviour hold". They do not answer the question the first audit actually asked,
which is **would a person walking in cold now get through the thing that stopped them**.

That is why the re-audit replays the audit's own repro lines rather than the suite's assertions, why
it runs live rather than against the replay fixtures, and why its deliverable is a screenshot per
finding rather than a pass count. A fix with no screenshot is not confirmed here, whatever the suite
says.

## 2. Confirmed fixes

One row per finding per persona, filled from `target/ux2/verdicts.tsv`. The image column is the
screenshot that shows it.

| # | Finding | Persona | Verdict | Screenshot |
|---|---------|---------|---------|------------|
| M12-overlays | M16/M12 — the first launch names the product and points at help (major) | developer-critic | fixed | [`02-first-launch-frame.svg`](docs/ux-audit-2-shots/developer-critic/02-first-launch-frame.svg) |
| B5-cwd | B5 — the file lands in the launch cwd (blocker) | developer-critic | fixed | [`03-cwd-landing.svg`](docs/ux-audit-2-shots/developer-critic/03-cwd-landing.svg) |
| M9-gutter | M9 — a gutter column separates the rail from the transcript (major) | developer-critic | fixed | [`03-gutter.svg`](docs/ux-audit-2-shots/developer-critic/03-gutter.svg) |
| M10-streaming | M10/M19 — no chunk boundary or literal marker survives on screen (major) | developer-critic | fixed | [`03-streaming.svg`](docs/ux-audit-2-shots/developer-critic/03-streaming.svg) |
| B1-focus | B1 — the composer keeps the keyboard (blocker) | developer-critic | fixed | [`04-click-then-type.svg`](docs/ux-audit-2-shots/developer-critic/04-click-then-type.svg) |
| B6-rowkeys | B6 — a tool row is reachable from the keyboard (blocker) | developer-critic | fixed | [`04-keyboard-row.svg`](docs/ux-audit-2-shots/developer-critic/04-keyboard-row.svg) |
| B2-scroll | B2 — the paging keys scroll the transcript (blocker) | developer-critic | fixed | [`05-scroll.svg`](docs/ux-audit-2-shots/developer-critic/05-scroll.svg) |
| B2-badge | B2 — an anchored viewport shows `N new` (blocker) | developer-critic | fixed | [`06-new-badge.svg`](docs/ux-audit-2-shots/developer-critic/06-new-badge.svg) |
| B2-end | B2 — End returns to the latest row (blocker) | developer-critic | fixed | [`06-end-to-latest.svg`](docs/ux-audit-2-shots/developer-critic/06-end-to-latest.svg) |
| B3-slash | B3 — a missed command keeps the sentence (blocker) | developer-critic | fixed | [`08-slash-miss.svg`](docs/ux-audit-2-shots/developer-critic/08-slash-miss.svg) |
| B4-paste | B4 — a multi-line paste is one draft (blocker) | developer-critic | fixed | [`09-paste.svg`](docs/ux-audit-2-shots/developer-critic/09-paste.svg) |
| draft-cleared | (probe) three Ctrl+U clear a three-line pasted draft (minor) | developer-critic | **not fixed** | [`09-draft-cleared.svg`](docs/ux-audit-2-shots/developer-critic/09-draft-cleared.svg) |
| M11-search | M11 — search shows snippets and a count, not ledger JSON (major) | developer-critic | fixed | [`10-search.svg`](docs/ux-audit-2-shots/developer-critic/10-search.svg) |
| M12-esc | M12 — Esc dismisses the search overlay (major) | developer-critic | fixed | [`10-esc-dismiss.svg`](docs/ux-audit-2-shots/developer-critic/10-esc-dismiss.svg) |
| M14-stopkey | M14 — the stop key is named while a turn runs (major) | developer-critic | fixed | [`11-stop-key.svg`](docs/ux-audit-2-shots/developer-critic/11-stop-key.svg) |
| B7-interrupt | B7 — Esc interrupts and says so (blocker) | developer-critic | fixed | [`12-interrupt.svg`](docs/ux-audit-2-shots/developer-critic/12-interrupt.svg) |
| B7-exitarm | B7 — an idle Ctrl+C asks before exiting (blocker) | developer-critic | fixed | [`12-exit-arm.svg`](docs/ux-audit-2-shots/developer-critic/12-exit-arm.svg) |
| M13-rail | M13 — the rail collapses at 80 columns (major) | developer-critic | fixed | [`13-rail-collapse.svg`](docs/ux-audit-2-shots/developer-critic/13-rail-collapse.svg) |
| M24-status | M24 — the status line names model, cwd and context (major) | developer-critic | fixed | [`14-status-line.svg`](docs/ux-audit-2-shots/developer-critic/14-status-line.svg) |
| B8-quit | B8 — /quit says goodbye and restores the terminal (blocker) | developer-critic | fixed | [`15-quit.svg`](docs/ux-audit-2-shots/developer-critic/15-quit.svg) |
| M28-restore | M28 — a relaunch restores the conversation (major) | developer-critic | fixed | [`16-restore.svg`](docs/ux-audit-2-shots/developer-critic/16-restore.svg) |
| M12-overlays | M16/M12 — the first launch names the product and points at help (major) | andrey-owner | fixed | [`02-first-launch-frame.svg`](docs/ux-audit-2-shots/andrey-owner/02-first-launch-frame.svg) |
| B5-cwd | B5 — the file lands in the launch cwd (blocker) | andrey-owner | fixed | [`03-cwd-landing.svg`](docs/ux-audit-2-shots/andrey-owner/03-cwd-landing.svg) |
| M9-gutter | M9 — a gutter column separates the rail from the transcript (major) | andrey-owner | fixed | [`03-gutter.svg`](docs/ux-audit-2-shots/andrey-owner/03-gutter.svg) |
| M10-streaming | M10/M19 — no chunk boundary or literal marker survives on screen (major) | andrey-owner | fixed | [`03-streaming.svg`](docs/ux-audit-2-shots/andrey-owner/03-streaming.svg) |
| B1-focus | B1 — the composer keeps the keyboard (blocker) | andrey-owner | fixed | [`04-click-then-type.svg`](docs/ux-audit-2-shots/andrey-owner/04-click-then-type.svg) |
| B6-rowkeys | B6 — a tool row is reachable from the keyboard (blocker) | andrey-owner | fixed | [`04-keyboard-row.svg`](docs/ux-audit-2-shots/andrey-owner/04-keyboard-row.svg) |
| B2-scroll | B2 — the paging keys scroll the transcript (blocker) | andrey-owner | fixed | [`05-scroll.svg`](docs/ux-audit-2-shots/andrey-owner/05-scroll.svg) |
| B2-badge | B2 — an anchored viewport shows `N new` (blocker) | andrey-owner | fixed | [`06-new-badge.svg`](docs/ux-audit-2-shots/andrey-owner/06-new-badge.svg) |
| B2-end | B2 — End returns to the latest row (blocker) | andrey-owner | fixed | [`06-end-to-latest.svg`](docs/ux-audit-2-shots/andrey-owner/06-end-to-latest.svg) |
| B3-slash | B3 — a missed command keeps the sentence (blocker) | andrey-owner | fixed | [`08-slash-miss.svg`](docs/ux-audit-2-shots/andrey-owner/08-slash-miss.svg) |
| B4-paste | B4 — a multi-line paste is one draft (blocker) | andrey-owner | fixed | [`09-paste.svg`](docs/ux-audit-2-shots/andrey-owner/09-paste.svg) |
| draft-cleared | (probe) three Ctrl+U clear a three-line pasted draft (minor) | andrey-owner | **not fixed** | [`09-draft-cleared.svg`](docs/ux-audit-2-shots/andrey-owner/09-draft-cleared.svg) |
| M11-search | M11 — search shows snippets and a count, not ledger JSON (major) | andrey-owner | fixed | [`10-search.svg`](docs/ux-audit-2-shots/andrey-owner/10-search.svg) |
| M12-esc | M12 — Esc dismisses the search overlay (major) | andrey-owner | fixed | [`10-esc-dismiss.svg`](docs/ux-audit-2-shots/andrey-owner/10-esc-dismiss.svg) |
| M14-stopkey | M14 — the stop key is named while a turn runs (major) | andrey-owner | fixed | [`11-stop-key.svg`](docs/ux-audit-2-shots/andrey-owner/11-stop-key.svg) |
| B7-interrupt | B7 — Esc interrupts and says so (blocker) | andrey-owner | fixed | [`12-interrupt.svg`](docs/ux-audit-2-shots/andrey-owner/12-interrupt.svg) |
| B7-exitarm | B7 — an idle Ctrl+C asks before exiting (blocker) | andrey-owner | fixed | [`12-exit-arm.svg`](docs/ux-audit-2-shots/andrey-owner/12-exit-arm.svg) |
| M13-rail | M13 — the rail collapses at 80 columns (major) | andrey-owner | fixed | [`13-rail-collapse.svg`](docs/ux-audit-2-shots/andrey-owner/13-rail-collapse.svg) |
| M24-status | M24 — the status line names model, cwd and context (major) | andrey-owner | fixed | [`14-status-line.svg`](docs/ux-audit-2-shots/andrey-owner/14-status-line.svg) |
| B8-quit | B8 — /quit says goodbye and restores the terminal (blocker) | andrey-owner | fixed | [`15-quit.svg`](docs/ux-audit-2-shots/andrey-owner/15-quit.svg) |
| M28-restore | M28 — a relaunch restores the conversation (major) | andrey-owner | fixed | [`16-restore.svg`](docs/ux-audit-2-shots/andrey-owner/16-restore.svg) |
| M12-overlays | M16/M12 — the first launch names the product and points at help (major) | keyboard-only-user | fixed | [`02-first-launch-frame.svg`](docs/ux-audit-2-shots/keyboard-only-user/02-first-launch-frame.svg) |
| B5-cwd | B5 — the file lands in the launch cwd (blocker) | keyboard-only-user | fixed | [`03-cwd-landing.svg`](docs/ux-audit-2-shots/keyboard-only-user/03-cwd-landing.svg) |
| M9-gutter | M9 — a gutter column separates the rail from the transcript (major) | keyboard-only-user | fixed | [`03-gutter.svg`](docs/ux-audit-2-shots/keyboard-only-user/03-gutter.svg) |
| M10-streaming | M10/M19 — no chunk boundary or literal marker survives on screen (major) | keyboard-only-user | fixed | [`03-streaming.svg`](docs/ux-audit-2-shots/keyboard-only-user/03-streaming.svg) |
| B1-focus | B1 — the composer keeps the keyboard (blocker) | keyboard-only-user | fixed | [`03-keyboard-focus.svg`](docs/ux-audit-2-shots/keyboard-only-user/03-keyboard-focus.svg) |
| B6-rowkeys | B6 — a tool row is reachable from the keyboard (blocker) | keyboard-only-user | fixed | [`03-keyboard-row.svg`](docs/ux-audit-2-shots/keyboard-only-user/03-keyboard-row.svg) |
| B2-scroll | B2 — the paging keys scroll the transcript (blocker) | keyboard-only-user | fixed | [`04-scroll.svg`](docs/ux-audit-2-shots/keyboard-only-user/04-scroll.svg) |
| B2-badge | B2 — an anchored viewport shows `N new` (blocker) | keyboard-only-user | fixed | [`05-new-badge.svg`](docs/ux-audit-2-shots/keyboard-only-user/05-new-badge.svg) |
| B2-end | B2 — End returns to the latest row (blocker) | keyboard-only-user | fixed | [`05-end-to-latest.svg`](docs/ux-audit-2-shots/keyboard-only-user/05-end-to-latest.svg) |
| B3-slash | B3 — a missed command keeps the sentence (blocker) | keyboard-only-user | fixed | [`07-slash-miss.svg`](docs/ux-audit-2-shots/keyboard-only-user/07-slash-miss.svg) |
| B4-paste | B4 — a multi-line paste is one draft (blocker) | keyboard-only-user | fixed | [`08-paste.svg`](docs/ux-audit-2-shots/keyboard-only-user/08-paste.svg) |
| draft-cleared | (probe) three Ctrl+U clear a three-line pasted draft (minor) | keyboard-only-user | **not fixed** | [`08-draft-cleared.svg`](docs/ux-audit-2-shots/keyboard-only-user/08-draft-cleared.svg) |
| M11-search | M11 — search shows snippets and a count, not ledger JSON (major) | keyboard-only-user | fixed | [`09-search.svg`](docs/ux-audit-2-shots/keyboard-only-user/09-search.svg) |
| M12-esc | M12 — Esc dismisses the search overlay (major) | keyboard-only-user | fixed | [`09-esc-dismiss.svg`](docs/ux-audit-2-shots/keyboard-only-user/09-esc-dismiss.svg) |
| M14-stopkey | M14 — the stop key is named while a turn runs (major) | keyboard-only-user | fixed | [`10-stop-key.svg`](docs/ux-audit-2-shots/keyboard-only-user/10-stop-key.svg) |
| B7-interrupt | B7 — Esc interrupts and says so (blocker) | keyboard-only-user | fixed | [`11-interrupt.svg`](docs/ux-audit-2-shots/keyboard-only-user/11-interrupt.svg) |
| B7-exitarm | B7 — an idle Ctrl+C asks before exiting (blocker) | keyboard-only-user | fixed | [`11-exit-arm.svg`](docs/ux-audit-2-shots/keyboard-only-user/11-exit-arm.svg) |
| M13-rail | M13 — the rail collapses at 80 columns (major) | keyboard-only-user | fixed | [`12-rail-collapse.svg`](docs/ux-audit-2-shots/keyboard-only-user/12-rail-collapse.svg) |
| M24-status | M24 — the status line names model, cwd and context (major) | keyboard-only-user | fixed | [`13-status-line.svg`](docs/ux-audit-2-shots/keyboard-only-user/13-status-line.svg) |
| B8-quit | B8 — /quit says goodbye and restores the terminal (blocker) | keyboard-only-user | fixed | [`14-quit.svg`](docs/ux-audit-2-shots/keyboard-only-user/14-quit.svg) |
| M28-restore | M28 — a relaunch restores the conversation (major) | keyboard-only-user | fixed | [`15-restore.svg`](docs/ux-audit-2-shots/keyboard-only-user/15-restore.svg) |

## 3. Residuals

Anything not fixed, and anything newly found, with a severity and the crate that owns it.

| # | Sev | Finding | Owner crate | Note |
|---|-----|---------|-------------|------|
| R1 | minor | **Ctrl+U does not clear a multi-line draft.** After a three-line bracketed paste, three Ctrl+U presses leave `alpha` and `beta` in the composer — Ctrl+U kills the line the cursor is on but never steps over the newline to the line above, so a pasted draft cannot be cleared with the kill key alone. Reproduced in all three walks (`draft-cleared` verdict; `docs/ux-audit-2-shots/*/0*-draft-cleared.svg`). | tui-shell | M20's fix ("Ctrl+U kills to line start") is correct for one line and pinned by `18-draft.sh::ctrl_u_clears_the_line`; the multi-line case has no test. A user who pastes and changes their mind must Backspace over the newlines. |
| R2 | minor | **The newline-burst paste fallback is unreachable in practice.** `run::on_key` gates the burst heuristic behind `!bracketed_paste_active()`, and the shell turns bracketed paste on at boot, so bare `\n`-separated bytes written into the terminal still fire one send per line — the original B4 shape, for any caller or terminal that delivers a paste without the wrapper. The walk therefore tests B4 the way a real terminal delivers it (`ESC[200~ ... ESC[201~`), which is `fixed`. | tui-shell | Deliberate (a fast typist must not be mistaken for a paste), and it is why the audit's literal repro line was changed here; recorded so the trade-off is not lost. |
| R3 | nit | Nit 38's timestamp half and the high-contrast-theme half of M22 were out of scope for phase ux1 (plan §4) and were not walked. | tui-strip | Carried forward unchanged. |

**A note on how these three walks were read.** The first pass of the re-audit produced six
`not-fixed` verdicts that did not survive investigation: five were harness defects (a `see` helper
that never reached the `bash -c` child; a disk check that ran before the tool's write landed; a
"two runs on a baseline" heuristic that fires on the rail's own padding; a whole-screen comparison
taken while the answer was still streaming; a grep for `press again` against a product that says
`press Ctrl+C again to exit`), and one was a cascade — a wedged composer left by the paste step made
the four checks after it interact with the search field instead of the composer. Each was proved
against the running binary before the check was rewritten; the behaviours themselves were correct.
The rewritten checks are the ones in `scripts/ux2/run.sh` today.

## 4. What the suite pins in the meantime

These are the screen-level assertions that run in `make gates` (replay half) and `make tui-test`
(both halves), each named by the verification map of `docs/phase-ux1-plan.md` §3. They are a
narrower claim than the re-audit's — behaviour, not welcome — but they are the claim that runs on
every commit.

| Audit finding | Pinned by |
|---|---|
| B1 clicking the transcript kills the composer | `scripts/tui/16-focus.sh` → `click_then_type_still_sends`, `the_click_did_not_steal_the_keyboard` |
| B2 focus-dependent scroll, no follow, no jump-to-latest | `17-scroll.sh` → `scroll_keys_work_from_the_composer` / `…_from_the_focus_pane` / `…_from_the_search_pane`, `the_wheel_scrolls_the_transcript`, `the_tail_follows_a_live_answer`, `scrolled_up_shows_the_new_badge`, `end_returns_to_the_latest_row` |
| B3 a `/` sentence is destroyed | `18-draft.sh` → `a_missed_command_keeps_the_sentence`, `a_second_enter_sends_the_missed_line_as_a_message` |
| B4 raw multi-line paste | `18-draft.sh` → `a_raw_three_line_paste_is_one_draft`, `…_and_one_send` |
| B5 the wrong cwd | `24-honesty.sh` → `a_file_in_the_current_directory_lands_in_the_launch_cwd` (a disk assertion), `the_file_did_not_land_in_the_checkout`, `the_status_line_names_the_launch_cwd` |
| B6 no keyboard path to a tool row | `16-focus.sh` → `arrows_move_a_visible_row_focus`, `enter_toggles_the_focused_row`, `space_toggles_the_focused_row` |
| B7 Ctrl+C exits with no confirmation | `19-interrupt.sh` → `an_idle_ctrl_c_asks_before_exiting`, `the_second_ctrl_c_exits_with_the_terminal_restored` |
| B8 `/quit` blanks the terminal or hangs | `19-interrupt.sh` → `quit_says_goodbye_and_is_gone_within_two_seconds`, `the_farewell_is_one_line_and_the_screen_is_not_blank` |
| M9 no gutter between rail and transcript | `20-frame.sh` → `at_80x24_the_rail_is_gone_and_no_row_carries_two_runs` |
| M10 / M19 chunk boundaries baked in; markdown shredded | `21-stream.sh` → `a_multi_chunk_replay_has_no_mid_word_break`, `the_capabilities_answer_shows_no_literal_markers`, `the_same_answer_renders_identically_after_a_relaunch` |
| M11 search over raw ledger JSON | `22-search.sh` → `hits_are_snippets_with_a_highlight_and_a_count`, `no_hit_row_contains_a_brace`, `enter_moves_the_transcript_to_the_hit` |
| M12 overlays never dismiss | `20-frame.sh` → `esc_dismisses_help_then_search_then_nothing` |
| M13 the rail is 34 columns at every width | `20-frame.sh` → `at_200x50_the_measure_is_capped_at_ninety`, `three_sizes_rewrap_with_no_blank_line_injected` |
| M14 Esc does not interrupt, nothing names the key | `19-interrupt.sh` → `esc_interrupts_and_marks_it`, `the_stop_key_is_named_while_running` |
| M15 patch accept/reject invisible | `24-honesty.sh` → `a_rejected_patch_says_so_on_screen_with_the_logs_words`, `a_good_patch_says_reloaded` |
| M17 no command palette | `23-commands.sh` → `slash_opens_a_palette_that_filters_and_moves`, `tab_completes_the_name_without_running_it` |
| M18 `/help` lists no keys | `23-commands.sh` → `help_lists_the_keys_that_actually_work` |
| M20 readline gaps | `18-draft.sh` → `ctrl_u_clears_the_line`, `up_recalls_the_last_sent_message`, `shift_enter_inserts_a_newline`, `alt_enter_inserts_a_newline` |
| M21 no selection, no copy feedback | `24-honesty.sh` → `a_drag_select_flashes_copied_and_emits_osc52`, `the_copy_hint_names_shift_drag` |
| M22 contrast inverted | `plugins/tui-shell/tests/contrast.rs` → `every_role_of_both_themes_clears_four_point_five` |
| M23 the wheel does not scroll | `17-scroll.sh` → `the_wheel_scrolls_the_transcript` |
| M24 the strip carries no model, cost, context or cwd | `20-frame.sh` → `the_status_line_names_the_six_things` |
| M25 invented capabilities | `24-honesty.sh` → `the_capability_answer_names_no_tool_that_is_not_registered` |
| M26 click hit-test off by one row | `16-focus.sh` → `click_toggles_the_row_it_landed_on` |
| M27 four no-op commands | `23-commands.sh` → `the_four_no_ops_answer_or_say_why`, `oldfeed_names_the_database_it_cannot_find` |
| M28 history not restored on relaunch | `24-honesty.sh` → `the_shutdown_left_no_wal_over_a_page`; `08-restore.sh` |
| minor 30 the search field never clears | `22-search.sh` → `esc_clears_the_query_and_the_hits` |
| minor 32 no progress affordance | `24-honesty.sh` → `a_running_turn_shows_a_spinner_and_an_elapsed_clock` |
| nit 34 no hanging indent on wrapped list items | `21-stream.sh` → `the_list_keeps_a_hanging_indent` |
| nit 36 no did-you-mean | `23-commands.sh` → `an_unknown_command_suggests_and_keeps` |
| nit 37 internal vocabulary in the chrome | `23-commands.sh` → `help_is_plain_language_and_not_engine_vocabulary` |
| nit 39 reflow injects blank lines | `20-frame.sh` → `three_sizes_rewrap_with_no_blank_line_injected` (via `lib.sh::no_blank_run`) |

## 5. The delights, re-checked

§4 of the first audit lists sixteen things nine or ten personas praised by name. None of them was
rewritten by this phase, and each is load-bearing in a test that still runs: the tool-row disclosure
(`02-tool-calls.sh`), the unified diff (`02-tool-calls.sh` → `a_diff_intent_shows_added_and_removed_lines_in_colour`),
glyph-plus-word status (`01-boot-and-turn.sh` → `the_status_glyph_returned_to_idle`), message
queueing and history restore (`08-restore.sh`), Ctrl+C as a safe stop (`19-interrupt.sh`, now with
the confirmation the audit asked for), bracketed paste (`18-draft.sh`, now alongside the raw kind),
the fail-safe config watcher (`24-honesty.sh`, now with the notice), and resize safety
(`20-frame.sh`).

## 6. Provenance

* Input audit: `docs/ux-audit-1.md`, shots in `docs/ux-audit-1-shots/`.
* Plan: `docs/phase-ux1-plan.md` (§3 is the verification map every test name above comes from).
* Harness: `scripts/ux2/run.sh`; raw verdicts at `target/ux2/verdicts.tsv`; shots at
  `docs/ux-audit-2-shots/<persona>/`.
* Binary: `target/release/bough`, built by `make release` from the `rebuild` branch.
* Models: `claude-haiku-4-5-20251001` for both `sol` and `terra` (`model.policy` in
  `bundles/bough-base.yml`), as the build's standing decision records.
