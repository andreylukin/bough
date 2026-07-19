<p align="center">
  <img src="assets/logo.svg" alt="bough" width="120" height="130">
</p>

<h1 align="center">bough</h1>

<p align="center">
  <b>A sandboxed coding agent with branchable history.</b><br>
  <i>A tree you tend in the dark — fork any point, and the filesystem forks with it.</i>
</p>

bough runs a frontier model as a **supervisor** that plans by writing code. A deterministic harness executes that code in a sealed V8 worker, confines spawned processes with a **macOS Seatbelt** profile, and routes outbound network through a default-deny egress gate. Every turn is a node in a tree you can **fork** — restoring the conversation *and* the files. TypeScript on Deno; a headless server driven by a full-screen terminal UI.

## Why bough

- 🌳 **Branchable history, branchable files.** Fork any earlier turn and the workspace reverts with it — explore a risky change, jump back if it goes wrong.
- 🔒 **Safe to leave running.** Every command runs under a kernel sandbox: workspace-only writes, `~/.ssh`/`~/.aws` and other secrets denied, network default-deny with a live allow/deny leash.
- ⚙️ **Code-mode.** The supervisor writes a small program each turn instead of one-shot tool calls — loops, composition, real control flow over the host bridge (shell, file ops, subagents, oracle, MCP, LSP).
- ✅ **Deterministic done.** A turn is "done" only when a committed `CHECK` command exits 0 — the harness re-runs it and stamps the verdict; the model can't declare victory on its own.

## Run

macOS only — the sandbox is Seatbelt-based. On a fresh machine, one line clones bough to `~/bough` (override with `BOUGH_DIR`) and runs the full setup: deps, worker model, API-key prompt, launchd service.

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
```

From an existing checkout the same setup is `scripts/bough setup`. The service starts at login and relaunches on crash, but never watches files — edits don't touch the running server until you restart. Manage it with:

```bash
bough                                  # the terminal UI (auto-starts the server)
bough start                            # serves http://127.0.0.1:4321 (logs: ~/.bough/server.log)
bough kill | restart | status | logs   # manage the service (kill sticks across logins)
bough update                           # fast-forward to origin/main, rebuild, restart
bough prompt [-w dir] [-m model] [--yolo] [--json] "..."   # headless one-shot turn
```

`bough prompt` runs a single turn against the running server and streams the answer to stdout — `--json` prints a machine-readable envelope with token usage (including cache splits) instead; exit code 0 means the turn finished clean.

Or run it in the foreground — requires [Deno](https://deno.com). From the repo root:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
deno task dev                          # serves http://127.0.0.1:4321
```

Anthropic is the default provider; adding an OpenAI or OpenRouter key unlocks their models in the picker (OpenAI ids are discovered live from `/v1/models`). Keys go in via the TUI (`^o` → API keys) or `PUT /config/keys`, and persist to `~/.bough/env`.

To expose the API remotely (artifact links, remote TUI), set a password and open a tunnel — with `BOUGH_PASSWORD` set the server also binds beyond loopback, and every request must log in:

```bash
BOUGH_PASSWORD=... deno task dev       # terminal 1
deno task tunnel                       # terminal 2 — prints a public trycloudflare URL
```

## Use it

Point a session at a repo and ask. The supervisor plans, the sandbox runs the work, and `CHECK` decides when it's done — every turn snapshotted, so history and files fork together. Drop an `AGENTS.md` in the repo root (or `~/.bough/AGENTS.md` globally) for standing project rules.

- **Branch anything.** Edit any past turn to fork from that point, compact a span of turns into a summarized branch, extract picked messages into a fresh conversation, or hand off a distilled goal prompt to a new session. LLM-labeled sections color the tree by topic; the heads map lays the whole tree out.
- **Review what changed.** The Changes view shows the run's uncommitted work as diffs — apply per file or revert before anything touches your tree for keeps. Or let the agent `ship()` its work: a commit (and optional push) into your repo, made with your own credentials, your staged files untouched.
- **Own the network.** The network panel is a live feed of gated egress; a request outside policy pauses as a *hold* you allow or deny. Installable policy **bundles** (e.g. `github`) grant scoped access with credential injection at the proxy, so tokens never enter the sandbox — and the policy editor can draft least-privilege rules from a plain-language ask.
- **Delegate and consult.** `agent()`/`spawn()` fork subagents as real branches of the tree, each in its own worktree, their diffs adopted explicitly; `oracle()` gets a read-only second opinion from a stronger reasoning model. `artifact()` publishes a file at a URL for browser viewing without polluting the diff.
- **Live in the terminal.** Full-screen TUI with one tabbed panel for everything that isn't chat — sessions, conversation tree, changes review, model/keys, net, MCP, skills, themes. `^t` toggles it, `^p`/`^f`/`^d`/`^o` jump straight to a tab, `esc` always backs out. Plus transcript search (`^s`), `@` file and `/` skill completion, `!` local shell, double-esc to clear or rewind-to-edit, mouse-expandable tool folds, clickable links, and desktop notifications and taskbar progress where the terminal supports them.
- **Local worker.** A small local model (Qwen2.5-Coder-3B via `llama-server`) handles the harness's micro-tasks for free — fast-apply edit reconciliation, long-output digestion, hold annotations, session titles. `BOUGH_WORKER_FRONTIER=1` routes those to `claude-haiku-4-5` instead (set it to a model id to pick another, or switch at runtime from the ⚒ worker picker); `BOUGH_WORKER_LOCAL_ONLY=1` pins everything local. Recall's semantic search embeddings always stay on-machine.

## How it works

- **One tool.** The supervisor's only tool is a JS program per turn, executed in a fresh Deno Worker with `permissions: "none"` — a sealed V8 isolate. Host functions bridge out: `bash` (plus background shells), `read`/`write`/`edit`, `oracle`, `artifact`, `agent`/`spawn`/`adopt`, `mcp`, `lsp.*`, `ship`.
- **Two fences.** The worker isolate confines the agent's program; a macOS Seatbelt profile confines the processes it launches — workspace-only writes, secret directories unreadable, and (while the proxy runs) all network denied except loopback, so the gate can't be bypassed.
- **Gated egress.** Outbound traffic is MITM'd per session and matched against policy; misses hold for a human verdict. Policies ship as parameterized bundles, extend with fixture-tested classifier plugins and CEL-style conditions, and inherit down the session tree.
- **Snapshots.** Each turn checkpoints the workspace into a per-project shadow git store — one isolated worktree per session, with the origin's history grafted in so `git log` and `blame` work, and `node_modules`/`.venv` hydrated via APFS clonefile so the code actually runs. Your repo's own `.git` is never touched; config (non-repo) sessions snapshot via clonefile.
- **MCP and LSP.** MCP servers are granted per session by skills (or explicitly), spawned under the same Seatbelt confinement, and every tool call passes the egress gate; OAuth for remote servers is built in. Symbol-navigation verbs (`lsp.*`) are bridged from the [leta](https://github.com/andreasjansson/leta) CLI when installed.
- **Headless server + TUI.** One Deno process serves the JSON API, an SSE event stream, and hosted artifacts on `:4321`; the `bough` terminal UI drives it (optional password gate for remote use). State lives in SQLite at `~/.bough/bough.db`.

## Develop

```bash
deno task test            # unit + integration tests
deno task check           # typecheck
deno task tui             # the terminal UI against the local server
```

`bench/` A/B-tests the harness against Claude Code on a fixed task bank — an oracle script grades the final workspace state, and the report prices cost per solved task. `probes/` measures TUI responsiveness and usability against the live server.
