# Changelog

## Unreleased (since v0.2.6)

### Web control room (`bough serve`)
- New web control room over bough sessions: sidebar tree of sessions and
  turns, ⌘K palette that searches transcripts, skill (`/`) and file (`@`)
  pickers, projects spanning several repos, phone layouts.
- Triage: Needs you, failed tests and unseen failures, hooks that ran,
  uncommitted changes, background jobs and prompt-cache warmth.
- Composer: paste images and large text, attach any file, drag and drop,
  failed sends kept with Retry/Edit.
- Background agents supervised by serve as child sessions, nested under
  their parent; a Work view for jobs and subagents.
- Wiki view: pages as cited claims, review, activity.
- Storybook and a design-system bundle for the UI.

### Sessions, orbs and projects
- Sessions start local (host home, read-only) or project (a container
  "orb" that writes), with project definitions, secrets by reference, a
  `/orb` skill and `bough project` for agents.
- Orbs act as the user, relay `bough mcp` to the host, build only on
  changed inputs, and show a live image build log.

### Agent loop and pipelines
- `bough loop`: deterministic pipelines with agent/check nodes, holdout and
  coach; plan and index skills.
- Every block records exit and duration; `/model` mid-turn hands the turn
  over; cost cap; fabricated `<system-*>` tags stripped from tool output.
- Headless: `--json` NDJSON events, correct exit codes on SIGINT and
  recovered failures, survives a closed stdout.

### Tools and code mode
- `tools.view`/`write`/`patch` hardening (binary files, CRLF, size caps,
  cross-agent locking); `tools.bash` scripts run from a file with stdin
  `/dev/null`; background-job fixes.
- Code mode: Promise rejections are errors, stack cap, panics recovered,
  `BOUGH_CODEMODE_TIMEOUT`.
- Subagent spawn/spawnAll failure paths fixed.

### Artifacts
- Agent-published pages hosted by bough, rewritten on OpenUI: interactive
  answers, patches, live reload, searchable index.

### History, search and wiki
- `bough search` over every session; resume and checkpoint robustness.
- LLM wiki compiled from session history (never injected into prompts).
- Session titles and a one-line log per turn.

### Plugins and config
- Claude Code and Codex rules honoured; `init.js` hot reload; hooks can
  talk to you, be interrupted with Esc, and show in the web UI.
- LLM providers retry rate limits and mid-stream errors with backoff.

### Removed
- All memory features (graph, collectors, auto-memory, memory tier,
  recipes, attention board) and the PR watcher.
- The `deploy/` droplet scripts and the `bench/tune` prompt tuner.

### Testing and CI
- Real-PTY (vtreal) and replay suites, end-to-end tests on the real
  binary, flaky-test cleanup, parallel CI jobs.
- Installer verifies release checksums and fails closed.
