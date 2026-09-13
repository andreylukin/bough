<p align="center"><img src="assets/logo-512.png" width="128" alt="bough logo"></p>

# bough

**A coding agent that acts by writing programs, where everything is a plugin.**

[![ci](https://github.com/andreylukin/bough/actions/workflows/ci-go.yml/badge.svg)](https://github.com/andreylukin/bough/actions/workflows/ci-go.yml)
[![release](https://img.shields.io/github/v/release/andreylukin/bough)](https://github.com/andreylukin/bough/releases/latest)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

The model has one tool: it writes JavaScript, bough runs it, and what the program prints goes back. A read, an edit and a test run can happen in one round trip. Everything else — the LLM provider, the loop, the tools, the UI, MCP, hooks, skills — is a row in a YAML config you can swap or hot-reload mid-session.

<p align="center"><img src="assets/screenshot-conversation.png" width="720" alt="bough transcript: reading, patching, and testing a Go file"></p>

More in [SCREENSHOTS.md](SCREENSHOTS.md).

> [!WARNING]
> There is no isolation boundary. Agent programs run as you, with your full authority.

## Install

macOS and Linux, x86-64 and arm64. One static binary.

```sh
curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh | sh
```

Or `brew tap andreylukin/bough https://github.com/andreylukin/bough && brew install bough`, or from source: `cd go && go build -o ~/.local/bin/bough ./cmd/bough` (Go 1.27+).

Add a key and run it:

```sh
mkdir -p ~/.bough && echo 'ANTHROPIC_API_KEY=sk-ant-...' >> ~/.bough/env   # or OPENROUTER_ / OPENAI_ / CEREBRAS_API_KEY
bough
```

Or start bough and use `/connect <provider> <key>`. `/model` switches models at any time. `bough update` rebuilds from `main`.

## Use it

Three front ends over the same sessions:

| | |
|---|---|
| `bough` | the terminal UI in the current directory (`-c` resume latest, `-r` pick) |
| `bough serve` | the web control room: every session, live, in a browser (`serve status` / `serve stop`) |
| `bough --headless` | stdin lines in, events out (`--json` for one object per line) |

In the TUI: `/` opens the command palette, `@path` attaches a file, `!cmd` runs a shell line, `esc` cancels a turn, `esc esc` rewinds, `ctrl+o` inspects history. `/init` writes an `AGENTS.md` briefing for a new repo. In the web composer you can paste images and long text; they attach as `[Image #N]` / `[Pasted text #N]` tags.

Other commands: `bough sessions`, `search`, `log`, `rows`, `mcp`, `wiki`, `sync-mcp`. `bough --help` lists them all.

## Configure

`./bough.yml` (else `~/.bough/bough.yml`) is laid over the embedded default by row id — list only the rows you change:

```yaml
- id: llm
  plugin: llm-openrouter
  config:
    model: deepseek/deepseek-chat
```

Saving the file reconciles the running process. The shipped tree is [go/bough.yml](go/bough.yml), commented row by row. Hooks live in `~/.bough/hooks/<event>/*.js`, skills in `~/.claude/skills/<name>/SKILL.md`, and `~/.bough/init.js` adds tools, commands, providers, keymaps and themes ([go/docs/INIT.md](go/docs/INIT.md)).

No telemetry. Besides your LLM provider and MCP servers, bough only fetches the public [models.dev](https://models.dev) price list, weekly.

## Develop

```sh
./.githooks/install
cd go && go build ./cmd/bough
go test -race -parallel 4 ./...       # unit, headless and PTY e2e
go test ./internal/vtreal             # real terminal (needs tmux)
cd internal/serve/web && bun install && bun run build   # web UI → dist/ (committed)
cd tests/web && npm ci && npx playwright install chromium && npm test
```

CI runs on Linux and macOS and cross-compiles Windows (unsupported). Writing a plugin: [go/docs/PLUGINS.md](go/docs/PLUGINS.md). Full reference: [go/README.md](go/README.md). Conventions and traps: [AGENTS.md](AGENTS.md).

## License

[Apache-2.0](LICENSE)
