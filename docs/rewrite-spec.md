# bough rewrite — decision record

Outcome of a full read of the current codebase (~40k lines, 54 commits) plus a
scoping interview. This records **what survives, what is cut, and why** — it is the
input to the rewrite, not a design doc for it.

Greenfield: new schema, new `~/.bough/bough.db`. The existing database is abandoned
(keep a copy read-only if you want the old sessions; nothing migrates).

## Fixed constraints

Not up for discussion — these were stated up front:

- **Server + client.** One headless server owning state and execution; clients drive
  it over HTTP + SSE.
- **Deno Workers for programmatic tool calling.** The supervisor acts by writing one
  JavaScript program per round, executed in a worker with host functions bridged in.
- **Thorough background agents, subagents, and workflows.** This is the part that
  gets built out rather than ported.

## What survives

### Execution model

| Thing | Note |
|---|---|
| One program per round in a Deno Worker | `permissions: "inherit"` — runs as the user |
| Host functions bridged over postMessage | Convenience and session integration, not a fence |
| Full Deno runtime inside the program | `Deno.Command`, sockets, `npm:`/`jsr:` imports |
| Turn checkpointing | Persisted state machine; a crash mid-turn resumes |
| In-place edits to the user's checkout | No shadow store, no overlay; `git diff <base>` is the review payload |

### Host-function surface

Shell: `bash`, `sh` (concurrent), `bashBg`, `bashOutput`, `bashWait`, `bashKill`
Files: `view`, `patch`, `write` — **one editing idiom, hash-anchored**
Delegation: `agent`, `spawn`, `join`, `adopt`
Orchestration: `workflow.start/status/stop/list/rerun`
Session: `ask`, `state.*`, `schedule.*`, `image`, `fetch`, `artifact`
Integration: `mcp`, `mcpStatus`, `lsp.*`

### Subsystems

- **MCP, in full** — registry, stdio spawn, remote Streamable HTTP, OAuth/PKCE with
  the hosted callback, per-session grants that subagents inherit.
- **LSP** — `lsp.*` verbs over the external `leta` CLI, prompted as the default for
  symbol-shaped work.
- **Workflows** — detached scripted fan-out in a `permissions: "none"` worker.
  Keeps the journal-backed selective rerun; **gains structured agent output**
  (`agent(prompt, {schema})` returning validated JSON instead of prose).
- **History as a tree** — fork / edit-and-resend, compact-as-branch, topic
  `sections`, extract, move-into, handoff. All six survive.
- **Artifacts** — hosted files on the server origin plus the injected comment layer
  that wakes the session with pinned notes.
- **Theming** — server-persisted palette, HTTP route, semantic token contract, live
  preview tab.
- **Skills** — `/name` loading, `${SKILL_DIR}`, per-skill MCP grants,
  `~/.bough/skills`. One bundled skill ships: **`history`**, repurposed to document
  how to query bough's own SQLite directly.
- **Cheap-model micro-tasks** — auto session titles, composer ghost text, live
  activity blurbs.
- **Model routing** — Anthropic, OpenAI, and OpenRouter all first-class, with the
  pricing catalog for live cost display. The cheap-tier model is **selectable in the
  model picker**, not hardcoded.
- **Clients** — the Ink TUI, decomposed: state layer separated from rendering, no
  3,600-line component.
- **Install** — one-line `install.sh` + launchd service. macOS-shaped, as today.

## What is cut

### Already-dead weight

These describe a bough that no longer exists. They are leftovers, not decisions:

- **The whole security narrative.** README sells a sealed V8 sandbox, Seatbelt
  confinement, a network default-deny leash, policy bundles with proxy-side
  credential injection, and `oracle()`. The network/MITM layer was deleted in
  `f7b8673`; the VM header now states outright that nothing there is a security
  boundary; `oracle` has zero references. **The rewrite has no isolation boundary
  and says so plainly.**
- **`docs/`** — `identity-boundary.md` (flagged, off, never cut over),
  `net-transparent-proxy.md` (marked "proposed — not built"), `mcp.md` (design draft
  still describing Seatbelt spawn and Claw Patrol gating that no longer exist).
- **`vcs/clonefile.ts`** — APFS snapshots of non-git config files, justified by a
  sandbox that is gone. Takes `POST /sessions/:id/changes/apply` with it; that route
  exists only to copy approved clones back.

### Cut by decision

- **The done-gate.** No committed `check`, no harness re-run, no gating of `done`.
  The `checkPassed` field on the `agent()` return goes away, as does the prompt
  section teaching it.
- **`recall()` and local embeddings.** Cross-session semantic search is replaced by
  keyword search (SQLite FTS) over transcripts. Drops the embeddings table, the
  lazy index-catchup, and nomic-embed.
- **The local `llama-server` tier.** No GGUF downloads, no supervised child
  inference processes, no `BOUGH_WORKER_*` env contract. Micro-tasks run on a
  frontier-cheap model chosen in the picker.
- **Output digestion and `extract()`.** Both were local-only-by-design privacy
  features; with the tier gone, neither is worth frontier tokens. Oversized program
  output is truncated deterministically (head + tail + omission marker).
- **`edit()` and `read()`.** One editing idiom only. Fast-apply edit repair dies
  with them — it existed solely to rescue `edit`'s exact-match failures.
- **Archive / deprecate / purge.** Replaced by **auto-collapse by lineage**:
  subagent and workflow-agent sessions fold into their spawner and surface on drill-in;
  roots always show. No manual hide action, no `archived_at`/`deprecated_at` columns,
  no purge route.
- **The research rig** — `bench/` (A/B harness and the overnight prompt tuner with
  its predictions ledger), `probes/`, and `metrics.ts` with its HTTP route and the
  `first_output_at` stamp. Also the `BOUGH_PROMPT_DIR` override that existed for the
  tuner.
- **json-render artifacts** — the `*.ui.json` component catalog, validation,
  registry, browser viewer bundle, and its served JS route.
- **Password gate + Cloudflare tunnel.** No remote access story; the server binds
  loopback.
- **Bundled skills `cloud`, `prewalk`, `tui-test`, `theme`.** The skill *mechanism*
  stays; these four were experiments.
- **Workflow budget ceilings and nesting.** Explicitly out of scope.

## Consequences worth holding

1. **Parallel background agents share one checkout, with nothing enforcing
   separation.** Worktree isolation and a file-lease broker were both considered and
   rejected in favor of today's model. Combined with the done-gate going away, a
   fan-out's only protection against two agents editing the same file is the
   spawner's prompt discipline — and its only signal that work succeeded is the
   agent's own prose report. `patch`'s hash anchoring is the one real safeguard left:
   it rebases when your lines are untouched and reports a conflict when they aren't.
   That makes it load-bearing, not merely preferred.
2. **Unattended runs have no machine-checkable success.** Schedules, detached
   `spawn()`s and workflow agents all report in prose now. Structured agent output
   (`{schema}`) is the partial answer — a script can at least branch on typed data.
3. **Big output now costs full context.** Without digestion, a 100KB build log is
   head+tail or nothing. The prompt's "filter at the source" guidance stops being
   advice and becomes the actual mechanism.
4. **Three micro-tasks bill on every round.** Titles, ghost text and activity blurbs
   were free when local. They now hit a paid API continuously, which is why the
   cheap model is picker-selectable — and why each should fail silently and never
   block a turn.
5. **Three provider paths to keep working.** Anthropic, OpenAI and OpenRouter drift
   independently; this is the one place the rewrite deliberately takes on more
   surface rather than less.

## Open questions

Small, unblocking, resolved by assumption unless you say otherwise:

- **`/theme` skill.** "Keep theming fully" bundled it, but nothing except `history`
  should be a skill. Resolved: theming survives as a subsystem; `/theme` does not
  ship as a skill.
- **`read()`.** Cut literally, per "view + patch + write only" — raw file content
  comes from `Deno.readTextFile` or `bash`. Say so if you want it back for
  non-source files.
- **`adopt()`.** Not covered by any question; carried forward with the rest of the
  delegation verbs.
- **`changes/revert`.** Survives as a git operation (restore tracked paths from the
  base sha, delete untracked). Only `changes/apply` dies with clonefile.
