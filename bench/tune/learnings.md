# Tuner learnings (persists across campaigns; newest last)

- [2026-07-21] Night 1 (tune-2026-07-20): 7 challengers, all check-discipline
  variations. Six refuted outright. iterative-check-execution ("RUN your check
  after each change") won 35/42 vs 32/42 but FAILED n=6 confirmation (13/24 vs
  12/24) — the refactor-behavior-preserving gain was pure variance. Do not
  re-propose bare check-execution phrasing changes.
- [2026-07-21] refactor-behavior-preserving is a ~33% coin-flip for haiku
  (pooled baseline 3/9), NOT a hard failure. Movement on it in a single sweep
  is noise. Same caution applies to migrate-format (~44%).
- [2026-07-21] Live suggestive signal worth a targeted hypothesis:
  migrate-format pooled 8/9 with the check-execution rule vs 4/9 baseline.
  If targeting it, diagnose from its failed transcripts, don't just rephrase
  check discipline.
- [2026-07-21] Output-mismatch fail counts did NOT drop under any of the six
  check-discipline variants (5 baseline -> 4-8 across variants). The bottleneck
  for output-mismatch is elsewhere — read transcripts before theorizing.
- [2026-07-21] transform-three-phase-verify (mutate): Decompose transformational verification into three isolated phases (test logic on simple example, apply to full dataset, verify output matches spec) to catch errors early and extend check-execution discipline benefits for migration and format-conversion tasks. -> 4/4 vs champion 4/4; paired stable +0 noisy +0 (bar 2.0) -> not promoted; moved: nothing
- [2026-07-21] explicit-edit-verification (mutate): Require supervisor to show before-and-after code or explicit change confirmation when making edits. This catches silent failures and wrong fixes before full verification runs waste tokens on broken fixes. -> 36/42 vs champion 34/42; paired stable +1 noisy +1 (bar 2.0) -> not promoted; moved: fanout-heavy, feature-config-priority, feature-pipeline, migrate-format
- [2026-07-21] requirement-first-check (mutate): Agents in failing bugfix/feature/migrate tasks loop without reading requirements or establishing checks. Move the check-definition phase to require reading requirements FIRST and defining success criteria from requirements—not from code you're about to write—so agents understand their target before making changes. -> 50/70 vs champion 47/70; paired stable +3 noisy +0 (bar 2.0) -> PROMOTED; moved: fanout-bugs, rename-precise, bugfix-inventory, refactor-rename, fanout-heavy, feature-topwords, bugfix-statemachine, feature-pipeline
- [2026-07-21] requirement-first-check end-of-night confirmation: confirmation sweep failed -> REFUTED — night gain was variance
