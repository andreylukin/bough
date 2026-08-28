# The plugin audit — phase c (REQUIREMENTS §17 Phase 8, §16)

The committed run of `make audit-plugins` (`scripts/audit-plugins.sh`) on branch `rebuild-c`.
Regenerate with `make audit-plugins`; `--json` prints the same table as JSON, `--bundle <name>`
audits another bundle, `--self-test` runs the classification rule against the recorded `--check`
reports in `scripts/fixtures/check-reports/` without booting anything.

**What the four phases assert**

| Phase | Claim | How |
| --- | --- | --- |
| A | every shipped profile composes AND boots whole | `bough --profile P --dump-config` then `--check` (compose, mount, quiesce, assert, tear down). An enabled row that never activates is a boot failure (§0.2). |
| B | no row of the bundle is load-bearing by accident | each row disabled by a generated patch, one at a time; classified `ok` (the tree settled) or `pending` (only DEPENDENTS waited, with their unmet keys named). A `Failed` row, a panic, a missing report, or a patch the launcher IGNORED is FAIL. |
| C | nothing LEAKS across a disable/re-enable | `cargo test -p bough --test audit_leaks`: binding and listener counts return to their pre-disable baseline. Asserted in-process because the launcher prints no such counts (D-C6). Its verdict is the `LEAKED` column of every phase-B row. |
| D | no two-Provider seam is welded to one Provider | each seam booted under each Provider by a patch that changes the row's `plugin`, then that seam's suite run under it. |

**The exceptions, each named (§16).** Four seams print `SKIP`, not `ok`, and the reason is the
table's own detail column:

- `llm` / `llm-anthropic` — the live Provider. Re-run with `BOUGH_LIVE=1` and a key.
- `projection` — one Provider. **This corrects the phase plan's §3.4:** `projection-probe` injects
  `projection` and contributes SECTIONS; it does not provide the key, so it is a Consumer and
  there is no second Provider to boot.
- `tui` — one Provider, same correction: `tui-probe` registers a PANE into `tui`.
- `workers` — one Provider: `worker-fork` is a second `WorkerKind` behind the same seam, not a
  swap of it.
- `actions` — one Provider today (`actions-shim`); the second arrives in Phase 6 (§7).

No other exception, and no row of either table is FAIL.

## The run — `bough-base`, phases A, B, C, D

```
   SUBJECT                DISABLED                PEND  FAIL LEAKED VERDICT  DETAIL
-- ---------------------- ---------------------- ----- ----- ------ -------- ------
A  profile:dev            -                          0     0      - ok       composes and boots
A  profile:headless       -                          0     0      - ok       composes and boots
A  profile:tui            -                          0     0      - ok       composes and boots
C  audit_leaks            -                          0     0     no ok       binding and listener counts return to baseline
B  bough-base             greeting.provider          1     0     no pending  1 dependent row(s) waiting on a key it provided
B  bough-base             hello.greeter              0     0     no ok       the tree settled without it
B  bough-base             ledger                    24     0     no pending  24 dependent row(s) waiting on a key it provided
B  bough-base             projection                 4     0     no pending  4 dependent row(s) waiting on a key it provided
B  bough-base             llm                        9     0     no pending  9 dependent row(s) waiting on a key it provided
B  bough-base             llm.anthropic              0     0     no ok       the tree settled without it
B  bough-base             llm.retry                  0     0     no ok       the tree settled without it
B  bough-base             tools                      9     0     no pending  9 dependent row(s) waiting on a key it provided
B  bough-base             tools.baseline             0     0     no ok       the tree settled without it
B  bough-base             agents                    15     0     no pending  15 dependent row(s) waiting on a key it provided
B  bough-base             agent.loop                 0     0     no ok       the tree settled without it
B  bough-base             model.policy               0     0     no ok       the tree settled without it
B  bough-base             about.line                 0     0     no ok       the tree settled without it
B  bough-base             workers                    5     0     no pending  5 dependent row(s) waiting on a key it provided
B  bough-base             worker.spawn               0     0     no ok       the tree settled without it
B  bough-base             tool.spawn_worker          0     0     no ok       the tree settled without it
B  bough-base             tool.ask                   0     0     no ok       the tree settled without it
B  bough-base             actions                    1     0     no pending  1 dependent row(s) waiting on a key it provided
B  bough-base             tool.actions               0     0     no ok       the tree settled without it
B  bough-base             rollups                    4     0     no pending  4 dependent row(s) waiting on a key it provided
B  bough-base             reconsolidation            0     0     no ok       the tree settled without it
B  bough-base             drift.watch                0     0     no ok       the tree settled without it
B  bough-base             mail                       2     0     no pending  2 dependent row(s) waiting on a key it provided
B  bough-base             dormancy                   0     0     no ok       the tree settled without it
B  bough-base             graph                      1     0     no pending  1 dependent row(s) waiting on a key it provided
B  bough-base             claims                     0     0     no ok       the tree settled without it
B  bough-base             lane.scope                 0     0     no ok       the tree settled without it
B  bough-base             worker.fork                0     0     no ok       the tree settled without it
B  bough-base             tool.fork                  0     0     no ok       the tree settled without it
D  seam:ledger            ledger-sqlite              0     0      - ok       boots; --test ledger_swap green
D  seam:ledger            ledger-sqlite              0     0      - ok       boots; --test ledger_invariants green
D  seam:ledger            ledger-memory              0     0      - ok       boots; --test ledger_swap green
D  seam:agent_loop        agent-loop                 0     0      - ok       boots; --test loop_swap green
D  seam:agent_loop        agent-loop                 0     0      - ok       boots; --test agent_invariants green
D  seam:agent_loop        agent-loop-scripted        0     0      - ok       boots; --test loop_swap green
D  seam:rollups           rollups-summarizer         0     0      - ok       boots; --test rollups_swap green
D  seam:rollups           rollups-summarizer         0     0      - ok       boots; --test memory_invariants green
D  seam:rollups           rollups-none               0     0      - ok       boots; --test rollups_swap green
D  seam:llm               llm-replay                 0     0      - ok       boots; --test agent_scripted green
D  seam:llm               llm-anthropic              0     0      - SKIP     live Provider: re-run with BOUGH_LIVE=1 and a key
D  seam:projection        projection-assembler       0     0      - SKIP     one Provider: projection-probe is a CONSUMER (it contributes sections), not a second Provider
D  seam:tui               tui-shell                  0     0      - SKIP     one Provider: tui-probe registers a PANE into tui, it does not provide the key
D  seam:workers           worker-spawn               0     0      - SKIP     one Provider: worker-fork is a second WorkerKind on the same Provider seam, not a swap
D  seam:actions           actions-shim               0     0      - SKIP     one Provider today; the second arrives in Phase 6 (§7)
```

`audit-plugins.sh` exits 0.

## The run — `bough-tui-app`, phase B

The terminal bundle's own rows, disabled one at a time against the `tui` profile.

```
   SUBJECT                DISABLED                PEND  FAIL LEAKED VERDICT  DETAIL
-- ---------------------- ---------------------- ----- ----- ------ -------- ------
B  bough-tui-app          commands                   5     0      - pending  5 dependent row(s) waiting on a key it provided
B  bough-tui-app          tui                        4     0      - pending  4 dependent row(s) waiting on a key it provided
B  bough-tui-app          tui.strip                  1     0      - pending  1 dependent row(s) waiting on a key it provided
B  bough-tui-app          tui.status                 0     0      - ok       the tree settled without it
B  bough-tui-app          tui.focus                  0     0      - ok       the tree settled without it
B  bough-tui-app          tui.search                 0     0      - ok       the tree settled without it
B  bough-tui-app          residents                  0     0      - ok       the tree settled without it
B  bough-tui-app          old-feed                   0     0      - ok       the tree settled without it
B  bough-tui-app          leader                     1     0      - pending  1 dependent row(s) waiting on a key it provided
B  bough-tui-app          tool.leader                0     0      - ok       the tree settled without it
```

The three digging panes of this phase (`tui.preview`, `tui.timeline`, `tui.drift`) are NOT rows of
this bundle — they are catalog rows in no bundle (D-C10 in `docs/track-c-merge-notes.md`), so
phase B cannot disable them. Their swap gate is `scripts/tui/30-swap-digging.sh`, which mounts all
three and then disables each one by a patch written while the binary runs.

## The classification rule, tested without a boot

`scripts/audit-plugins.sh --self-test` runs `classify` against six recorded `--check` reports:

```
ok - classify dependents.rc1.pending.txt -> pending
ok - classify failed.rc1.FAIL.txt -> FAIL
ok - classify ghost-row.rc1.FAIL.txt -> FAIL
ok - classify panic.rc101.FAIL.txt -> FAIL
ok - classify settled.rc0.ok.txt -> ok
ok - classify silent.rc1.FAIL.txt -> FAIL
```
