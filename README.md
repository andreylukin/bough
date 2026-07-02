<p align="center">
  <img src="assets/logo.svg" alt="bough" width="120" height="130">
</p>

<h1 align="center">bough</h1>

<p align="center">
  <b>A sandboxed coding agent with branchable history.</b><br>
  <i>A tree you tend in the dark — fork any point, and the filesystem forks with it.</i>
</p>

<p align="center">
  <img src="assets/screenshots/hero.png" alt="bough running a task — the sealed-VM program expanded, CHECK passed" width="900">
</p>

bough runs a frontier model as a **supervisor** that plans by writing code, which a deterministic harness executes inside a **kernel-enforced sandbox** — each turn's program runs in a sealed V8 worker, spawned processes are confined by a **macOS Seatbelt** profile, and outbound network passes through a default-deny egress gate with a live allow/deny leash. Every turn is a node in a tree you can **fork** — restoring the conversation *and* the files. Written in **TypeScript on Deno**, served as a headless server with a React web UI.

## Why bough

- 🌳 **Branchable history, branchable files.** History is a tree. Fork any earlier turn and the workspace reverts with it — explore a risky change, jump back if it goes wrong.
- 🔒 **Safe to leave running.** Every command runs under a kernel sandbox: workspace-only writes, `~/.ssh`/`~/.aws` and other secrets denied, network default-deny with a live allow/deny leash.
- ⚙️ **Code-mode.** The supervisor writes a small program each turn (`bash`/`read`/`write`/`edit`) instead of one-shot tool calls — loops, composition, real control flow.
- ✅ **Deterministic done.** A turn is only "done" when a committed `CHECK` command exits 0 — not when the model says so.

## Run

Requires [Deno](https://deno.com). From the repo root:

```bash
cd bough-next
export ANTHROPIC_API_KEY=sk-ant-...
deno task dev                          # serves http://127.0.0.1:4321
```

To drive it from a phone, set a password (auth on, LAN bind) and open a tunnel:

```bash
BOUGH_PASSWORD=... deno task dev       # terminal 1
deno task tunnel                       # terminal 2 — prints a public trycloudflare URL
```

<p align="center">
  <img src="assets/screenshots/mobile.png" alt="the same session driven from a phone" width="340">
</p>

## Use it

Point a session at a repo and ask. The supervisor plans, the sandbox runs the work, and a deterministic `CHECK` decides when it's done — every turn snapshotted so you can fork the history and the files together.

- **Branch anything.** Edit any past turn to fork from that point, or compact a span of turns into a summarized branch; the heads map lays the whole tree out.
- **Review what changed.** The Changes rail shows the run's uncommitted work as diffs — apply per file or revert, before anything touches your tree for keeps.

<p align="center">
  <img src="assets/screenshots/changes.png" alt="reviewing a run's diff before applying it" width="900">
</p>
- **Own the network.** The Network rail is a live feed of gated egress; a request outside policy pauses as a *hold* you allow or deny from the UI. Installable policy **bundles** (e.g. `github`) grant scoped access with credential injection, so tokens never enter the sandbox.
- **Local worker.** A small local model (Qwen2.5-Coder-3B via `llama-server`, fetched by `scripts/worker-model.sh` into `~/.bough/models`) handles delegated fixes for free, gated by `CHECK`.

## How it works

- **One tool.** The supervisor's only tool is a JS program per turn, executed in a fresh Deno Worker with `permissions: "none"` — a sealed V8 isolate; only `bash`/`read`/`write`/`edit` bridge out.
- **Two fences.** The worker isolate confines the agent's program; a macOS Seatbelt profile confines the processes it launches — secrets unreadable, writes confined to the workspace (+ toolchain caches).
- **Gated egress.** Outbound traffic is captured and matched against policy; misses hold for a human verdict. Policies ship as parameterized bundles.
- **Snapshots.** Each turn checkpoints the workspace ([jj](https://github.com/jj-vcs/jj) for repos, APFS clonefile for config); a fork restores it.
- **Headless server + web UI.** One Deno process serves the JSON API, an SSE event stream, and the built React UI on `:4321` — same origin, phone-friendly, optional password gate for remote use.

## Develop

```bash
cd bough-next
deno task test     # unit + integration tests
deno task check    # typecheck
cd web && npm run build   # rebuild the web UI (server serves web/dist)
```

`make serve` / `make test` / `make check` at the root are shorthands for the same.
