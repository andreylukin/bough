# Install

## Requirements

macOS or Linux, and a terminal. Windows is not supported; WSL works.

There are no prebuilt binaries — installing builds from source, so the first run
compiles the workspace and takes a few minutes.

## Install

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
```

That clones into `~/bough` (override with `BOUGH_DIR`) and runs `scripts/bough setup`,
which:

- installs Rust via rustup, and `node`, `ripgrep` and `uv` from Homebrew / `apt-get` /
  `dnf` / `pacman`
- builds the release binary
- links `bough` into `~/.local/bin`
- writes `~/.bough/env`

Already have a clone? Run `scripts/setup.sh` directly.

**Optionally install `bun`.** Programs run under `bun` when it is on PATH and `node`
otherwise. Setup installs only `node`, so the default install takes the fallback path —
`Bun.file`, `Bun.$` and faster process starts come with `bun`.

## Keys

```bash
$EDITOR ~/.bough/env
```

```
ANTHROPIC_API_KEY=sk-ant-…
OPENAI_API_KEY=…          # optional
OPENROUTER_API_KEY=…      # optional
CLOUDFLARE_API_TOKEN=…    # optional
```

Models route to a provider by their id prefix, and the model picker (`^o`) lists what
the server's keys can actually reach — not a compiled-in catalog. At least one key is
required for a turn to run.

Two tiers: the **frontier** model runs turns, and a **cheap** model handles titles,
ghost text and activity blurbs. The cheap tier fails silently when it cannot reach a
model, so a missing optional key costs you polish, not function.

## Run

```bash
bough start     # background service: starts at login, restarts on crash
bough           # the TUI (auto-starts the server if it is down)
```

The service manager is the only platform-specific piece — launchd on macOS, a systemd
**user** unit on Linux, a plain background process where there is no user systemd
(containers, WSL1).

## Update

```bash
bough update    # fast-forward origin/main, rebuild, restart
```

There is no file watcher. If you are editing bough's own source, changes land only on
an explicit `bough restart`.

## Uninstall

```bash
bough kill                 # stop the service and keep it stopped across logins
rm ~/.local/bin/bough
rm -rf ~/bough             # the clone
rm -rf ~/.bough            # data: sessions, history, artifacts, config
```

Removing bough does not touch any repository it has worked in. Your code and its git
history are yours and were never copied anywhere — that is what "in place" means.

`bough purge` is unrelated to uninstalling: it deletes sessions archived more than 30
days ago and needs the server running.
