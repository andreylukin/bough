# Track C — merge notes

What the digging panes, the plugin audit and the hardening tests needed from seams they do not
own, what deviates from the phase plan, and what is still open. Written at integration; every
"proven" claim below names the test that ran green.

## Files this track had to touch outside its own crates

| File | Edit | Why |
| --- | --- | --- |
| `crates/bough/Cargo.toml` | five `workspace = true` dependency lines | a plugin crate that is not linked has no `inventory::submit!` in the binary, so its rows are "not in the catalog" |
| `crates/bough/src/lib.rs` | five `use … as _;` lines | same reason: the linker drops an rlib nothing references, and the launcher never names a plugin type |
| `bundles/bough-tui-app.yml` | a COMMENT, not rows — see D-C10 | the three panes are catalog rows in no bundle |
| `plugins/tui-drift/src/lib.rs` | `Inject::required` gained `"ledger"` | the row READ `ledger` without declaring it and the kernel refused it at boot (`--check` on the `tui` profile) |
| `Makefile` | `events` target | §15 item 7's gate needs an invocation |

Nothing else under an existing crate's `src/` was edited.

## Decisions and honest limits

**D-C8 — an anchored preview does not reproduce a past wake's `projection_digest`.** This is the
one plan bullet that did not land as written, and the test that was supposed to prove it is what
disproved it. `crates/bough/tests/preview_bytes.rs` boots the headless tree, runs a real wake, and
compares. What holds: the pane's text IS `Assembled::to_text()` of the same `assemble` call at the
same `as_of` — the pane re-spells nothing (asserted). What does not hold: the same call replayed
after the wake returns different bytes and even a different SECTION LIST (`["identity","tail",
"mail"]` during the wake, `["identity","about-line","tail"]` after). The cause is in `projection`,
not in the pane: not every section is a pure function of the ledger below `as_of` — `mail` renders
the message being answered, `about-line` renders a row written after the wake ended.

*The hook wanted:* sections that read live state should read it AT `as_of` (or declare themselves
unreplayable). When they do, tighten the gate — the test carries an `assert_ne!` on the digests
with a message saying exactly that, so it fails the moment the gap closes and nobody has to
remember.

**D-C1 (unchanged) — Head states its delta.** `PreviewAt::Head` prints `+3 preface rows at wake`;
`delta::only_preface` is what makes that number a rule rather than a guess.

**D-C9 — the preview passes the wake through.** `snapshot()` originally called `assemble` with
`wake: None`, which the plan called "the loop's own defaults"; the loop passes
`wake: Some(spec.wake)` (`plugins/agent-loop/src/wake.rs`, step 6). At an anchored `as_of` the
preview now resolves the wake that owned the newest step at or below it. At head there is no wake
yet, and `None` is the honest value.

**D-WP5-1 (as `tui-search` recorded it) — pane key handling is a pure function.** `PaneCx` /
`RenderCx` / `TuiHandle` are only constructible inside `tui-shell`, so each pane's keys are a pure
`on_key(key, &mut State, painted) -> …` that the `…PaneArc` wrapper delegates to. That is what
makes the key bullets testable at all.

**Esc is not a dismiss for a band pane.** `tui-shell::run::dismiss_overlay` hands the keyboard back
to the composer regardless of the pane's outcome, and an `Slot::Aux` pane stays on screen. The
three shell-use scripts assert what actually happens (the keyboard returns; the timeline's editor
clears first), not a dismissal this row does not have. *Hook wanted, unchanged from WP-3:*
`dismiss_overlay` should skip `give_keyboard_to_composer()` when the pane returns
`PaneOutcome::Handled`.

**D-C-WP2-1 — `Filter::describe()` prints ABSOLUTE times.** `describe(&self)` takes no `now` and
the crate reads no clock, so `since:2h` cannot be re-derived; it prints `since:<rfc3339>`. Add a
`now` argument at merge, or accept absolute.

**D-C6 — the leak audit is in-process, over a NAMED event set.** `KernelCore` exposes
`listener_count(&'static str)` but no enumeration of registered event names, so
`crates/bough/tests/audit_leaks.rs` walks a hand-listed set of `E::NAME` constants. The BINDING
half has no such limit (`binding_count()` is the whole tree's). If `xtask events` grows a
machine-readable catalog, generate that list from it.

**Fault sites.** `FaultSite::WakeStopping` cannot "fail": `agent/wake-stopping` is SERIAL with
`Output = Infallible` (P2-D10), so `how: error` records and logs; only `how: panic` breaks the
listener. A real failure channel would be a change in `plugins/agents`.

**D-C10 — the three panes are in the catalog and in NO bundle (the `tui-probe` precedent).** They
were rows of `bough-tui-app.yml` first, and `scripts/tui/01-boot-and-turn.sh` failed:
`Slot::Aux` splits ONE column band between its panes, so three more always-present panes squeezed
the focus pane down to nothing and the streaming bullet could not see a partial answer. Each
digging script now mounts its own pane with `add_row`, and a person digging mounts one by patch.

*The hook wanted:* an Aux policy where a pane costs the band only while it is open (the way an
overlay does), so a digging pane can ship on by default. That is a `tui-shell` change — not this
track's to make, and the reason the rows are commented out rather than deleted.

**Two tick storms, found by the same script.** `tui-preview` armed an `assemble` and `tui-timeline`
a 400-step-per-agent read on EVERY tick. Both now hold an in-flight flag and a `due(now, ms)`
debounce (`PreviewState::due`, `TimelineState::due`), matching `tui-drift`'s `poll_due`. Worth a
look at merge: a pane that reads on `Tick` and does not debounce is a footgun the `Pane` trait
does not guard against.

## Still open (NOT done, not claimed)

- **Three shell-use bullets the plan named and this branch does not have.** `33-preview.sh` has no
  `the_pane_scrolls_with_the_wheel` and no `at_34_rows_the_preview_costs_nothing` (the script mounts
  the pane at `collapse_rows: 24`, so 34 rows is above its breakpoint, not below — `34-timeline.sh`
  carries the breakpoint bullet instead, at 20 rows); `34-timeline.sh` has no
  `clicking_a_row_focuses_that_agent_and_step`. The pane-level click behaviour IS covered offline by
  `tui-timeline`'s `pane::tests` (`on_click` → `FocusRequest`); what is missing is the screen-level
  proof that a mouse report reaches it.
- **Bullet NAMES differ from the plan's list.** `27`/`28`/`29` were written by WP-1..WP-3 against
  the surface they found (`the_command_opens_the_preview_pane` rather than
  `the_preview_opens_on_slash_preview`, and so on). Same claims, different spellings; the phase
  plan's §5 verification map cites the plan's names and should be re-pointed at the scripts' own
  at merge.

## WP-7's hooks (integration: the audit, the swap script, the table)

**The audit's two-provider sweep landed, and it corrects the plan's §3.4.** `scripts/audit-plugins.sh`
now runs four phases (A profiles, B every bundle row disabled one at a time, C `audit_leaks`, D every
two-Provider seam booted under each Provider with that seam's suite), prints one table, supports
`--json`, `--bundle <name>`, `--phases`, and `--self-test`, and exits non-zero on any FAIL. Its
committed run is `docs/plugin-audit-c.md`.

What §3.4 got wrong: only FOUR seams on this branch have two Providers — `ledger`
(`ledger-sqlite`/`ledger-memory`), `agent_loop` (`agent-loop`/`agent-loop-scripted`), `rollups`
(`rollups-summarizer`/`rollups-none`) and `llm` (`llm-anthropic`/`llm-replay`). `projection-probe`
and `tui-probe` are CONSUMERS — they inject the seam and contribute a section or a pane; neither
provides the key — and `worker-fork` is a second `WorkerKind` behind the same Provider, not a swap
of it. Those three, plus `actions` (one Provider until Phase 6) and the live `llm-anthropic` arm,
print as named `SKIP` rows with the reason, never as `ok`.

**H-C8 — a patch naming a row no layer created was passing the audit as `ok`.** The launcher reports
`layer … names row `x`, which no layer created` and then boots the ORDINARY tree; the previous
script read that exit status as "the tree settled without it". Six of the plan's row ids are spelled
differently in the bundle (`ledger`, not `ledger.sqlite`), so this was not hypothetical. `classify`
now treats that line as FAIL, and `--self-test` pins it against a recorded report
(`scripts/fixtures/check-reports/ghost-row.rc1.FAIL.txt`).

**H-C9 — the swap script measures the BAND's top edge, not a pane's row.** `pane::layout` anchors
the `Aux` band at the bottom, so a pane leaving moves the panes ABOVE it down and leaves the ones
below exactly where they were. The first version of
`scripts/tui/36-swap-digging.sh::disabling_the_preview_row_removes_it_and_the_layout_reflows`
asserted that the pane below moved and failed for that reason. What every reflow bullet asserts now
is that the band's top edge drops — the same statement as "the focus pane above it grew", and true
for whichever of the three left.

## WP-5's hooks (hardening tests, written and green)

`crates/bough/tests/{crash_reconcile,failure_injection,spawn_storm,audit_leaks}.rs` are all
written and passing. Three things they had to work AROUND rather than fix, because this track may
not edit an existing crate's `src/`. Each is a one-line change at merge.

**H-C5 — `spawn_worker` is unusable on the shipped tree.** `tool-workers` reads the `workers` seam
off `ToolCx.ctx`, which is the LOOP's context, and `agent-loop::inject()` does not declare
`workers`. Every `spawn_worker` call through a real wake comes back

```
workers seam unavailable: plugin `agent-loop` (row `agent.loop`) read service `workers`
without declaring it in inject
```

`spawn_storm.rs::every_refusal_reaches_the_model_as_a_tool_result_failure` patches
`agent.loop`'s entry inject to add the key (§0.3's entry ∪ plugin-static) so the case can run at
all. *The fix:* add `workers` to `agent-loop`'s static inject, or to the `agent.loop` row in
`bundles/bough-base.yml`. The same shape applies to any tool that reaches a seam through `ToolCx`
rather than holding a handle the way `tool-actions` does — which is why `open_pr` works and
`spawn_worker` does not.

**H-C6 — `fault-inject`'s agent filter cannot match at `wake_stopping`.** The site filters with
`AgentName::new(p.agent.as_str())`, but `AgentWakeStopping`'s `agent` is an `AgentId` (a uuid), so
`agent: sol` silently matches nothing there. `failure_injection.rs` therefore sets no agent filter
on that site. *The fix:* resolve the name from the handle the payload already carries, or take the
filter over `AgentId`.

**H-C7 — an OPTIONAL inject creates no ordering edge.** `fault-inject` declares `projection`,
`tools` and `agents` optional, so a row inserted at the root end can `apply` before `projection`
has committed its binding and dies with "the `projection` seam is not mounted". The fault rows
name the seam required at the ENTRY level to order themselves. Worth deciding at merge whether an
optional key should still order (it reads as a footgun) or whether `fault-inject` should declare
these required and gate on the site instead.

**A reader outside the process must declare the tree's step types.** `SqliteStore` refuses to READ
a step type it was not told about (`UnknownStepTypeOnRead`), so `crash_reconcile.rs` registers the
vocabularies of nine plugin crates before it can read a chain the binary wrote. That is §3 working
as designed, but a `vocabulary::all()` helper (or one exported by the launcher) would keep the
list from going stale every time a row adds a step type.

## Integration run (the gate)

- `cargo build --workspace --all-targets`, `cargo clippy -D warnings`, `cargo fmt --check`,
  `cargo test --workspace`: green.
- `docs/event-catalog.md` was STALE at integration (WP-5's new hardening tests added two listen
  sites: `kernel/listener-failed` 1→2, `kernel/rows-unresolved` 0→1). Regenerated with
  `cargo run -p xtask -- events --write docs/event-catalog.md`; `xtask::the_committed_catalog_matches_the_tree`
  is green. This is exactly the merge hazard WP-6 predicted — run it again after the track-B merge.
- `make tui-test-replay`: every script green EXCEPT
  `scripts/tui/19-interrupt.sh::the_farewell_is_one_line_and_the_screen_is_not_blank`. NOT a
  track-C regression. That script exits the binary twice in one PTY session (Ctrl+C exit, then a
  relaunch and `/quit`), so the primary buffer's scrollback carries two `bough: bye.` lines and the
  script's own `[ "$n" -eq 1 ]` sees 2. It passes only when earlier output happens to scroll the
  first farewell past the `tail -20` window, which is why it reads as flaky under load.
  `plugins/tui-shell/src/run.rs::farewell()` is untouched by this track. Reported to the ux1 track;
  the file is not edited from this worktree to avoid a merge conflict.
- Track C's own screens re-run standalone at integration, all green: `33-preview.sh` 5/5,
  `34-timeline.sh` 6/6, `35-drift.sh` 6/6, `36-swap-digging.sh` 5/5.
- `./scripts/audit-plugins.sh --self-test`: 6/6.
- No `todo!()` or `unimplemented!()` anywhere under `crates/` or `plugins/`.

## Reconciliation run (after the parallel verification pass)

Four defects stood between this branch and a green `make gates`. Three are in ux1-owned test
scripts and one is in this track's own test file; all four are fixed here, and the two ux1 script
edits are small and self-contained so the merge can take either side knowingly.

- `crates/bough/tests/crash_reconcile.rs` (track C's own): the throwaway `$BOUGH_HOME` was named
  from the variant tag, the pid and the clock. Three cases share a tag and run concurrently in one
  test binary, so two entering `Home::new` inside one clock tick shared a directory and the second
  `remove_dir_all` deleted the first's home out from under a live child. Symptoms were "the
  rendered layer is writable: NotFound" and "the child exited normally instead of being killed".
  Fixed with a per-`Home` counter. Green under the full `make gates` afterwards.
- `scripts/tui/19-interrupt.sh::the_farewell_is_one_line_and_the_screen_is_not_blank` (ux1): the
  known double-`bough: bye.` read. The assertion now looks only at the screen AFTER the last echo
  of the `/quit` launch line, so the earlier Ctrl+C exit's farewell is out of the window. The
  product is unchanged.
- `scripts/tui/23-commands.sh::slash_opens_a_palette_that_filters_and_moves` (ux1): `he` matches
  exactly one command, and on a one-row palette `Down` correctly changes nothing — the bullet
  asserted movement AFTER the filter, so it failed on a palette that was behaving. Movement is now
  asserted on the unfiltered palette, filtering after it. Deterministic failure, not a flake.
- `scripts/tui/23-commands.sh::enter_accepts_the_palette_selection` and
  `an_unknown_command_suggests_and_keeps` (ux1): both were latent behind the bullet above and had
  never run. The first pressed Enter on a palette whose selection was the FIRST row (`/accept`) and
  then looked for the word `help` — which the `/help` palette row put on screen anyway, so the old
  form was vacuous either way; it now narrows to `/he` first and asserts the help BODY
  (`shift+enter`). The second typed `/hepl` and pressed Enter as two steps.

PRODUCT FINDING for the ux1 track (not fixed here — `tui-shell` is not this track's to edit):
with the palette open on a query that matches NOTHING, a standalone `Enter` is swallowed and the
line is never submitted. `shell-use submit` (type and Enter in one go) still sends it, which is why
`05-commands.sh` never saw this. From a user's seat: type `/hepl`, press Enter, nothing happens.

Two race-shaped flakes were observed under load and are NOT fixed (they belong to crates this
track may not edit); each reproduced once in five full gate runs and never in isolation:
- `crates/bough/tests/exec_headless.rs::exec_runs_one_task_end_to_end_with_llm_replay` failed once
  with `no agent factory is set; mount an agent-loop row` — the `exec` row requires the `agents`
  SERVICE, but the factory slot is filled during the `agent-loop` row's activation, so the two
  orders are both legal and only one works. 5/5 green when the test binary runs alone.
- `scripts/tui/05-commands.sh` failed once at boot with `lane.scope ... read optional service
  projection, which no active fiber provides`. 3/3 green in isolation. Same shape: an activation
  order that is legal and unlucky.

## Merge outcome (rebuild-c → rebuild, 2026-08-28)

**Files** touched by this section's fixes: `plugins/tui-timeline/src/{filter.rs,pane.rs,command.rs}`,
`plugins/fault-inject/src/lib.rs`, `crates/bough/src/vocabulary.rs`, `crates/bough/Cargo.toml`,
`crates/bough/tests/{audit_leaks.rs,crash_reconcile.rs,failure_injection.rs,spawn_storm.rs,docs.rs}`,
`crates/bough/tests/fixtures/crash.patch.yml`, `docs/{phase-c-plan.md,event-catalog.md}`,
`bundles/bough-tui-app.yml`, `Makefile`, `Cargo.toml`.

One line per hook the notes above raise.

| Hook | Outcome |
| --- | --- |
| **H-C5** `spawn_worker` is unusable on the shipped tree | **IMPLEMENTED**, and it is the same finding as the code-mode branch's merge note §7. `agent-loop` and `agent-loop-scripted` declare `workers` as an OPTIONAL inject key, so a tool that resolves a seam from `ToolCx.ctx` is no longer refused. `spawn_storm.rs`'s `agent.loop: inject: {optional: [workers]}` row patch is GONE — the transcript runs against the shipped inject, which is the only way it can measure the shipped bounds. `crates/bough/tests/workers_seam.rs` proves a worker spawns under BOTH consumers and greps the tree so a new tool cannot repeat it. |
| **H-C6** `fault-inject`'s agent filter cannot match at `wake_stopping` | **IMPLEMENTED.** The name comes off the live `Agent` handle the payload already carries (`p.handle.name()`), not from `AgentName::new(p.agent.as_str())` over an `AgentId`. `failure_injection.rs::an_agent_filter_matches_by_name_at_the_wake_stopping_site` asserts both sides — a filter naming the agent fires, one naming a different agent does not. |
| **H-C7** an OPTIONAL inject creates no ordering edge | **DECIDED, not changed.** Optional stays non-ordering: that IS the difference between the two, and a tree where an optional key ordered would make every row wait for a seam it can live without. The sanctioned way to order is what `fault.patch.yml` already does — name the seam REQUIRED at the entry (§0.3's entry ∪ plugin-static), which is per-deployment and visible in the layer. The merge leaned on the same rule from the other side (`agent-loop`'s new `workers` is optional precisely because nothing in `apply` reads it), so the two halves are consistent. `fault_row`'s doc comment carries the reasoning. |
| **A reader outside the process must declare the tree's step types** | **IMPLEMENTED** — the note asks for "a `vocabulary::all()` helper (or one exported by the launcher)" and it is the launcher's, `bough::vocabulary::all()`. The hand-list went stale at this very merge (it missed `program/*` and `schedule/*`, and a store that has not been told a type reads a chain as EMPTY rather than as an error — four `crash_reconcile` cases went red on it). `vocabulary::tests::every_plugin_that_writes_steps_is_in_the_list` greps `plugins/` so it cannot go stale again, and `the_whole_list_registers_into_one_map` catches a name clash between two definitions. |
| **D-C6** the leak audit is over a hand-listed event set | **IMPLEMENTED.** `xtask events` DID grow the machine-readable catalog the note hoped for, so `audit_leaks.rs::watched()` reads `docs/event-catalog.md` and watches EVERY event in the tree, not five. `make events` is the gate that the catalog matches the tree. |
| **D-C-WP2-1** `Filter::describe()` prints ABSOLUTE times | **IMPLEMENTED** — the note's first option, "add a `now` argument at merge". `describe(now)` spells `since`/`until` as the span the person typed whenever the instant is an EXACT whole multiple of a day, an hour or a minute (or of a second under a minute), and as RFC3339 otherwise, so the round trip through `parse_filter` is exact and the header says what the editor line above it says. `render_filter`'s `now`, which was `_now` and did nothing, means the same thing now. Gate: `filter::tests::a_whole_span_is_described_the_way_it_was_typed`. |
| **Bullet NAMES differ from the plan's list** | **IMPLEMENTED.** `docs/phase-c-plan.md` §4 and §5 are re-pointed at the scripts' own bullet names, and `crates/bough/tests/docs.rs::every_shell_bullet_the_phase_c_map_names_exists` reads the map and the scripts so they cannot drift apart again. The three bullets that are still NOT SHIPPED are named in §4 rather than left to be rediscovered. |
| **D-C8** an anchored preview does not reproduce a past wake's `projection_digest` | **DEFERRED.** The hook wanted is "sections that read live state should read it AT `as_of` (or declare themselves unreplayable)" — a `projection` change touching every section, with its own gates. `preview_bytes.rs` carries the `assert_ne!` on the digests with a message saying exactly that, so it fails the moment the gap closes; nobody has to remember. |
| **Esc is not a dismiss for a band pane** (`dismiss_overlay` should skip `give_keyboard_to_composer()` on `PaneOutcome::Handled`) | **DEFERRED.** It is a `tui-shell` key-path change, and the parallel `ux-visual` track owns that path and has just rewritten the Esc arms in `tui-search` and `tui-status` (pass A). Changing it under them at a merge is how two tracks fight. The three scripts assert what actually happens rather than a dismissal the row does not have, so nothing here is dishonest. |
| **D-C10** the three panes are in the catalog and in NO bundle | **DEFERRED, and the comment stays in `bundles/bough-tui-app.yml`.** Turning them on by default needs an `Slot::Aux` policy where a pane costs the band only while it is open — again `tui-shell`, again the ux track's. Each digging script mounts its own pane by patch, as does a person digging. |
| **PRODUCT FINDING: a standalone Enter is swallowed on a no-match palette** | **RECORDED for the ux track, not fixed here.** `23-commands.sh` sends the miss with `shell-use submit`, which is track C's own workaround and the one `05-commands.sh` has always used. The product bug — type `/hepl`, press Enter, nothing happens — is real and belongs to `tui-shell`. |
| **Fault sites: `WakeStopping` cannot "fail"** | **UNCHANGED, as recorded.** `agent/wake-stopping` is SERIAL with `Output = Infallible` (P2-D10), so `how: error` records and logs and only `how: panic` breaks the listener. A real failure channel is a `plugins/agents` change. |

### What the merge itself had to fix

**`crates/bough/tests/fixtures/crash.patch.yml` disables `actions.github`.** Track C wrote this
fixture when `actions` had no Provider; track B's `actions.github` is in `bundles/bough-base.yml`
now and registers `open_pr` too. With both mounted the SHIM is not what runs the act, so the two
configured delays that ARE this file's kill windows never happened: the wake ended before the kill
landed (three cases found no `wake/end reason=interrupted`) and `until_gh_called` waited out its
240 s deadline against a shim nobody had called. One `disabled: true` in the layer, and the
fixture's subject — the reconciliation, against the shim it reconciles — is back.

**`docs/event-catalog.md` was stale**, exactly as WP-6 predicted and this file asks for
("run it again after the track-B merge"). Regenerated; `xtask::the_committed_catalog_matches_the_tree`
is green.

**The scripts are renumbered.** `27`/`28`/`29`/`30` were taken by track B's `27-drafts`,
`28-mcp-tool`, `29-swap-collector`, `30-swap-wards` and by code mode's `31-program` /
`32-codemode-swap`, so this track's four are `33-preview`, `34-timeline`, `35-drift`,
`36-swap-digging`. Every citation in `BUILD.md`, `docs/phase-c-plan.md`, `docs/plugin-audit-c.md`
and this file moved with them.
