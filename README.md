<p align="center">
  <img src="assets/logo.svg" alt="bough" width="120" height="120">
</p>

<h1 align="center">bough</h1>

<p align="center">
  <b>A coding agent that acts by writing programs.</b><br>
  One JavaScript program per round — loops, branching, composition — run against your real checkout.
</p>

<p align="center">
  <a href="https://github.com/andreylukin/bough/actions/workflows/ci.yml"><img src="https://github.com/andreylukin/bough/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="Apache-2.0"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg" alt="macOS | Linux">
</p>

<p align="center">
  <img src="assets/screenshot-conversation.png" alt="bough conversation" width="820">
</p>

**bough** rhymes with *now*, not with *dough* — /baʊ/. It is the word for a branch of a tree, which
is what a conversation is here: you fork a turn and the old line goes on living as a branch.

Most harnesses let the model emit one tool call and wait. bough gives it a single tool that takes a
program: the model writes JavaScript with real control flow, and a harness executes it on your
machine. A headless server owns all state and execution; the terminal UI is a view over it.

bough is an alternative harness **design**, not a better coding agent. That distinction is the point
of the project, and this README tries not to blur it.

> [!WARNING]
> **There is no isolation boundary.** Programs run as you, with your full authority — filesystem,
> network, subprocesses, `npm:` imports. No sandbox, no egress proxy, no credential gating. Host
> functions are convenience and session integration, never a wall.
>
> This is a deliberate choice, not an unfinished one ([spec §2](docs/spec.md)): the harness edits
> your real files because reviewing `git diff` and pushing with your own git is the delivery
> mechanism. Run it only on a machine where you would be comfortable running the code it writes,
> because that is exactly what happens.

## The idea

- **One program per round.** The model's only action is `run_steps(code)`. Control flow lives in the
  program, not in a chain of round-trips.
- **In place.** The agent edits your own checkout — no copy, no overlay. The Changes rail is
  `git diff` against the sha the session started from; you deliver with `git commit` / `git push`.
- **History is a tree.** Fork any turn, compact a span onto a new branch, lift messages into a fresh
  root. Nothing is destructively rewritten — every operation produces a new branch.
- **The server is the system.** State, execution, and orchestration are server-side. A client can
  crash or detach without affecting a running turn.
- **Delegation is core.** Subagents and workflows are primary capabilities with real persistence,
  lifecycle control, and observability.

A round looks like this — one program that scans, fans out, and reports, where another harness
would spend five round-trips:

```js
// Which crates still pin the old ratatui, and do they still pass?
const pinned = (await bash("rg -l 'ratatui = \"0.29\"' crates", "repo:scan:ratatui"))
  .trim().split("\n").filter(Boolean);

const names = pinned.map(p => p.split("/")[1]);

// sh() runs them concurrently; a non-zero exit is data, not an exception.
const runs = await sh(names.map(n => ({
  cmd: `cargo test -p ${n} 2>&1 | tail -3`,
  tag: `cargo:test:${n}`,
})));

const broken = names.filter((_, i) => runs[i].code !== 0);
console.log(broken.length ? `failed: ${broken.join(", ")}` : "all green");
```

The loop, the fan-out and the branch are the model's own code. `console.log` is what streams to
you and what comes back as the round's result, so the program decides what is worth your context —
here, the names that failed rather than four test logs.

## Install

macOS or Linux. Builds from source — the first run takes a few minutes.

```bash
brew tap andreylukin/bough https://github.com/andreylukin/bough
brew install bough
$EDITOR ~/.bough/env      # ANTHROPIC_API_KEY=…
bough start               # background service
bough                     # the TUI
```

Without Homebrew, the same install as a script — it clones into `~/bough` and builds there:

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
```

**Models.** Four providers, and a model routes to one by the shape of its id alone:
`claude-opus-5` is Anthropic, `openai:gpt-5` is OpenAI's Responses API, `vendor/model` is
OpenRouter, `@cf/vendor/model` is Cloudflare Workers AI. OpenRouter is the wide door — if it
carries a model, bough can run a turn on it. Every provider's base URL is overridable, and the
OpenRouter path speaks `/v1/chat/completions`, so pointing `OPENROUTER_API_BASE` at Ollama,
vLLM, LM Studio or a gateway runs turns against that instead. The picker (`^o`) lists what your
keys actually reach rather than a compiled-in catalog, and any one key is enough to start.

Full instructions, keys, and updating: [docs/install.md](docs/install.md).

## Use it

Point a session at a repo and ask in plain language. bough writes a small program, runs it, and
answers — folded reasoning, the code that ran, live cost and context in one view. Unfold a step
(`^e`) and you see the actual program and its output:

<p align="center">
  <img src="assets/screenshot-program.png" alt="an unfolded step: the program that ran, and its output" width="820">
</p>

Everything that is not the conversation lives in one panel with nine tabs, each on a direct-jump
chord — the conversation tree (`^f`), changes (`^d`), workflows (`^w`), model (`^o`), MCP (`^p`),
skills (`^k`), hooks (`^x`), context (`⌥c`), theme (`^y`). Press `?` for the full keymap.

Review with `^d`: the Changes rail is `git diff` against the sha the session started from, per file
and revertable per path. You commit and push with your own git.

<p align="center">
  <img src="assets/screenshot-changes.png" alt="the changes rail" width="820">
</p>

Rewind to any turn and send something else, and the old line survives as a branch.

<p align="center">
  <img src="assets/screenshot-tree.png" alt="the conversation tree with a branch point" width="820">
</p>

→ [docs/tui.md](docs/tui.md) for panels and every chord · [docs/cli.md](docs/cli.md) for `exec`,
`acp`, `mcp`, `tags` and the rest

## What it can do

**Programs.** Eighteen host functions in scope, plus the full JS runtime. One editing idiom —
`view` gives numbered lines with a version tag, `patch` names lines instead of quoting them, so code
being edited never has to survive the model's own string escaping, and a stale edit reports a
conflict instead of clobbering. → [docs/programs.md](docs/programs.md)

**Delegation.** `agent` and `spawn` run subagents in the same checkout; a workflow is a detached
orchestration script with `parallel` / `pipeline` primitives, schema-validated results, and a
journal that replays unchanged work on rerun instead of paying for it twice.
→ [docs/delegation.md](docs/delegation.md)

**Memory across sessions.** Every shell command carries tags naming what it is *for*, written at the
moment the command is written. A session opens primed with its project's own vocabulary, and
`bough tags` answers what was tried here, what worked, and what it printed.
→ [docs/tags.md](docs/tags.md)

**Extending it.** Skills, Lua hooks that can start work rather than only veto it, JavaScript
extensions bound into every program's scope, and MCP as a command rather than a verb. Reads the
`AGENTS.md`, `CLAUDE.md` and `.claude/skills` your other harnesses already wrote.
→ [docs/extending.md](docs/extending.md)

## Documentation

[**docs/**](docs/) is the map. Start at [install.md](docs/install.md), then
[tui.md](docs/tui.md). [how-it-works.md](docs/how-it-works.md) is the architecture in one page;
[spec.md](docs/spec.md) and [specs/](specs) are authoritative for behavior.

## What bough is not

These are decisions, not gaps:

- No confinement of any kind, and no credential gating.
- No acceptance gate — the model reports what it did and you verify it. The harness does not re-run a
  committed command or block a turn from finishing.
- No local inference in the turn loop; the cheap tier is a hosted model. The one exception is the
  embedding layer, which runs a small model inside SQLite.
- No embeddings over transcripts — cross-session transcript search is SQLite FTS. The two vector
  indexes cover the tagged command memory and note sections; both live in a separate
  `embeddings.db` that is derived state and can be deleted at any time.
- No per-agent worktrees or file leases. One shared checkout.
- No remote access, no auth layer, no web UI.

## Contributing

The most useful contributions sharpen or falsify the design. Read
[CONTRIBUTING.md](.github/CONTRIBUTING.md) for setup, the bar for a pull request, and the
verification you are expected to have done.

Bugs and features go through the [issue templates][issues]; questions and design debates belong in
[Discussions][discussions]. Security issues go through [SECURITY.md](.github/SECURITY.md), never the
public tracker. Participation is governed by the [Code of Conduct](.github/CODE_OF_CONDUCT.md).

[issues]: https://github.com/andreylukin/bough/issues/new/choose
[discussions]: https://github.com/andreylukin/bough/discussions

## License

[Apache License 2.0](LICENSE). By contributing you agree your contributions are licensed under it;
there is no CLA.
