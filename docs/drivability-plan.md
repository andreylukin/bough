# Drivability Plan — grilled 2026-08-31

Goal unchanged: the Phase 3 gate — one full real workday through the new TUI. This plan is
fix-forward (no seam reopening); all five symptoms are in scope. Decisions below were grilled
with Andrey on 2026-08-31 and are settled, not proposals.

## Diagnosed symptoms → decisions

### 1. Input disappears after submit
- **Observed:** every send; input vanishes from screen but the agent responds → the ledger has
  the event, the timeline never renders it. Pure projection/TUI bug, deterministic.
- **Decision:** immediate local echo on submit, reconciled when the ledger event lands. Also fix
  the underlying non-render of the user-message event.

### 2. Inconsistent, hard-to-follow text
- **Observed:** all four failure shapes at once — formatting varies by event type, streaming
  re-renders differently than final, attribution unclear, ordering/interleaving confusing. Both
  visual and structural. Bites in **timeline and panel**. Ledger text is fine — **the renderer
  owns this**, not the model.
- **Decision:** one coherent render pass, not incremental patching. An **event-type → rendering
  contract** is written as a design doc/mockup and approved by Andrey **before** implementation.
  The idiom is **bough's own design** (Claude Code is an input, not the target).
  **Streaming must render identically to final — no re-render jump.**
- **Ungrillable (prototype):** the contract's actual look (judged on the mockup), and
  streaming-identical rendering feasibility (technical spike in the renderer).

### 3. Agent weak at day-to-day functions
- **Observed:** same model as old bough → harness regression. Failure shapes: missing context +
  weak execution. Must-work tasks for the Phase 3 workday: **code edit → commit → push**,
  **morning catch-up / re-entry**, **PR / review work**.
- **Decision:** **codemode stays** (no A/B vs baseline). Land injection (item 5) first — much of
  "weak" is presumed missing context — then measure the residual on scripted tasks.

### 4. Pushes don't work
- **Observed:** agent `git push` errors. `--local` vs resident not yet compared; detached
  resident missing ssh-agent/gh auth is the prime suspect.
- **Decision:** if it's the resident env, the fix shape is **resident inherits a login-shell
  environment snapshot** (SSH_AUTH_SOCK, PATH, gh) — not creds in config.

### 5. No agents/CLAUDE.md/skills injection
- **State:** unknown whether `prompt-files` / `skills` / `hooks-exec` rows are uncomposed,
  stubs, or reading wrong sources → audit first.
- **Decision — Claude Code parity, scoped:**
  - **CLAUDE.md**: walk-up parity (`~/.claude` + project `.claude`), verbatim behavior.
  - **Skills**: parity including **installed Claude Code plugin skills**, not just
    `~/.claude/skills` + project skills.
  - **Hooks**: **full hook-event parity** (PreToolUse/PostToolUse/Stop/…), not just the
    currently configured hooks.
  - **Subagents (`agents/`): explicitly OUT** — no need.
  - MCP servers and settings permissions: not in scope for this pass.
- Priority within this item: CLAUDE.md + skills first (they drive the day-to-day failures).

## Verification
- **Scripted first, then Andrey.** Each fix is proven by scripted/replay sessions
  (`agent-loop-scripted`, `llm-replay`); a real driven workday validates the batch.
- Scenario sources: **both** — replays mined from real failed sessions in `~/.bough/bough.db`
  (regressions) and hand-written scenarios for the new contracts (render, injection).

## Status (2026-08-31, same-day)

Diagnoses done (input render path, injection audit, push path). Landed and tested:
- **W1** — user rows bypass the context plan's mail/show hiding (`tui-focus/src/lib.rs` paint,
  tray counts third-party mail only); regression test in `tui-focus/tests/render.rs`. Echo is
  immediate via the synchronous `inbox/spliced` append — no optimistic machinery needed.
- **W2 (env half)** — resident spawns with a login-shell env snapshot
  (`crates/bough/src/attach.rs`, `$SHELL -lc 'command env -0'`, terminal-set values still win).
  OPEN: the boundary/surface contradiction — `boundary-instructions` bans raw `git push` while
  the codemode shell surface demos it, and `push_to_pr` refuses ordinary pushes (marker trailer,
  open-PR precondition). Needs a decision on the sanctioned path for ordinary branch pushes.
- **W3 CLAUDE.md** — `prompt-files` grew `home` + `walk_up` discovery (`~/.claude/<file>`,
  ancestors outermost-first, `<root>/.claude/<file>`); on in `bough-base`.
- **W3 skills** — `triggers` optional (catalog-only skills), `SKILL.md` directory layout +
  symlinks, extra `roots` (`~/.claude/skills`, `~/.claude/plugins`) walked recursively and
  watched; host contributes a catalog section + a `skill` load tool (ledgered result); codemode
  exposes it automatically. Trigger-skills keep auto-injecting.
- **W3 hooks** — new `hooks-parity` row (in `bough-base`): reads BOTH Claude Code
  (`settings.json`/`settings.local.json` under `.claude`) and Codex (`hooks.json` /
  `config.toml [hooks]` under `.codex`) hook settings; discovery is per TOOL CALL from the
  call's own cwd walked to the root (a command run anywhere inside a project finds that
  project's hooks) plus the user layer. PreToolUse = real deny/ask on the `tools/pre-execute`
  waterfall (exit 2 + stderr, `permissionDecision` deny/ask, legacy `decision: block`);
  PostToolUse blocks or attaches `additionalContext` on `tools/post-execute`. Matchers try the
  raw bough name AND a parity alias (`bash`→`Bash`, `read_file`→`Read`, …). `only`/`except`
  (command substrings) toggle individual hooks; skills got the same toggles by name, and the
  pool dir (`$BOUGH_HOME/skills`) now carries both flat `*.md` and `<name>/SKILL.md` layouts.
  REMAINING: post-hoc parity events (Stop, SessionStart, UserPromptSubmit, …) — no per-call
  cwd; they belong on the ledger-step machinery (`hooks-exec` points) as a follow-up. Codex
  `mcp_tool` hooks and `updatedInput` rewrite are not supported (bough's §9 has no input
  rewrite by design).
- Known flaky: ~6 `cargo test -p bough` integration tests fail under the full parallel run and
  pass individually — pre-existing, not from these changes.
- **Stuck updates, diagnosed 2026-08-31**: `bough update`/`restart` wedged because the running
  resident's tokio IO driver stopped being polled — `sample` showed NO kevent thread, every
  worker in `park_condvar`, main parked in `block_on` — so the SIGINT was never heard, and the
  old "never SIGKILL" policy made that terminal. Fix: restart escalates SIGINT (15s) → SIGTERM
  (5s) → SIGKILL (3s), bounded and reported (REQUIREMENTS §0.1 revised). OPEN: root cause of
  the driver loss — seen on the 5cdc6fd1-era binary; if a post-fix resident wedges the same way
  (`sample <pid>` shows no kevent), it is still live and worth a real hunt.

### Follow-along fixes (2026-08-31 evening, after the GLM conversation diagnosis)
- **Dialogue band** (`projection-assembler`): the newest `dialogue_steps` (12) conversation
  steps older than the tail stay verbatim, so a tool-heavy wake (6–9 steps per codemode call)
  cannot evict the thread. `0` = off; goldens unchanged. TUI treats its steps as in-context.
- **Sealing scheduled at all**: `/seal` had NO schedule row — five days, zero rollups. New
  `schedule.seal` row (same `schedule-reconsolidate` plugin, job names now `system:<command>`),
  every 30 min with catch-up.
- **Pending retries** (`schedule-cron`): a `Pending` run (command racing the registry at boot —
  why `system:reconsolidate` never ran) retries on `pending_retry_ms` (60s) instead of waiting a
  whole cadence.
- OPEN: `cache_read_tokens: 0` on the OpenRouter/GLM path — caching not engaging; teardown-order
  grumble `provider=projection dependent=boundary` at shutdown (seen in `--check` 20:41).

## Workstream order
1. **Diagnose in parallel** (read-only audits): input→timeline render path; injection row
   state; push/exec path + resident spawn env.
2. **W1 input echo** — smallest, deterministic; local echo + fix the missing render.
3. **W2 pushes** — compare `--local` vs resident; land the login-env snapshot fix.
4. **W3 injection** — CLAUDE.md walk-up → skills (incl. plugins) → full hooks parity.
5. **W4 render contract** — write the design doc/mockup, get approval, implement as one pass
   (timeline + panel; streaming == final).
6. **W5 execution quality** — after W3: scripted bench (replays + hand-written) over codemode;
   judge the residual.
