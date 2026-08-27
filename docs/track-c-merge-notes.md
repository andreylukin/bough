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

- **WP-5's hardening tests.** `crates/bough/tests/{crash_reconcile,failure_injection,spawn_storm}.rs`
  are still `#[ignore]`d stubs — the kill-9/reconcile, fault-injection and spawn-storm bullets are
  UNPROVEN. `audit_leaks.rs` is written and green. The building blocks they need
  (`fault-inject`, `actions-shim`) are written, green, and in the catalog.
- **The audit's two-provider sweep.** `scripts/audit-plugins.sh` boots every profile and takes every
  `bough-base` row out one at a time (29 rows, table + non-zero exit). It does NOT yet boot each
  seam with each of its two providers and run that seam's suite; `loop_swap.rs` covers that seam
  today, the others do not.
