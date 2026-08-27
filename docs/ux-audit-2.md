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

> **Status: not yet run against a complete V11 tree.** This document's tables are written by the
> run, not by hand. Until `make ux2` has executed end to end on a tree where every work package of
> phase ux1 has landed, the confirmed-fix table below is EMPTY and the residuals table carries the
> one honest row it can: that the re-audit has not been performed. Nothing here is a claim about a
> screenshot that does not exist. See "Provenance" at the foot of the document.

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
| — | _(the run has not been executed; see Status above)_ | — | — | — |

## 3. Residuals

Anything not fixed, and anything newly found, with a severity and the crate that owns it.

| # | Sev | Finding | Owner crate | Note |
|---|-----|---------|-------------|------|
| R1 | blocker | The re-audit itself has not been run end to end. | — (process) | WP-8 wrote the harness (`scripts/ux2/run.sh`), the nine screen scripts and the swap script, and verified the row wiring and `--dump-config`. The live three-persona walk requires a tree in which every phase-ux1 work package has landed; at the time of writing it had not — a boot with the scroll fixture left the typed message in the composer with no answer and no status row, and `scripts/tui/01`, `02` and `03` were red. Run `make ux2` and regenerate sections 2 and 3 from `target/ux2/verdicts.tsv`. |

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
