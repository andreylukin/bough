# Install

## Requirements

macOS or Linux, and a terminal. Windows is not supported; WSL works.

There are no prebuilt binaries. Installing builds from source, so the first run
compiles the workspace and takes a few minutes.

## Install with Homebrew

```bash
brew tap andreylukin/bough https://github.com/andreylukin/bough
brew install bough
```

The formula is [`Formula/bough.rb`](../Formula/bough.rb) in this repository, which is why the
tap takes an explicit URL: `brew tap user/name` alone looks for `user/homebrew-name`. Copy the
same file into a repository called `homebrew-bough` and the one-liner
`brew install andreylukin/bough/bough` works instead, auto-tapping as it goes.

It builds the same source as everything else here, brings `node`, `ripgrep`, `uv` and
`ast-grep` as dependencies, and puts one `bough` on PATH, the same command with the same
verbs as below.

Two things differ from the script install, both because a package prefix is not a checkout:

- **`bough update` does not apply.** It says so and names `brew upgrade bough`, which is
  what actually replaces the binary. `BOUGH_REF` is a checkout's channel selector and has
  no meaning here.
- **There is no source tree to work in.** If you want to edit bough itself, use the script
  install (or clone separately); the formula ships a binary, not a repo.

Pick one or the other. If you already have the script install, its `bough` is on PATH
already and `brew install` will report the formula as **not linked** rather than replace
it, which is the right outcome, not an error to fix: `brew link --overwrite bough` would
point the name at the package and leave the checkout's service manager driving a binary it
no longer owns. To switch, remove the old symlink (`scripts/setup.sh` put it in
`~/.local/bin` or `/opt/homebrew/bin`) and then `brew link bough`.

`bough start` still installs bough's own LaunchAgent / systemd user unit. It is deliberately
not a `brew services` formula: bough already manages that lifecycle on both platforms, and
two service managers pointed at one server is a way to have neither work.

## Install with the script

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
```

That clones into `~/bough` (override with `BOUGH_DIR`) and runs `scripts/bough setup`,
which:

- installs Rust via rustup, and `node`, `ripgrep` and `uv` from Homebrew / `apt-get` /
  `dnf` / `pacman`
- installs `ast-grep` (structural search, which the system prompt names unconditionally)
  via brew, cargo or npm, whichever is present
- builds the release binary
- links `bough` into `~/.local/bin`
- writes `~/.bough/env`

Already have a clone? Run `scripts/setup.sh` directly.

**Which commit you get.** The newest release tag, not the tip of `main`. bough builds
from source, and `main` is a branch that gets pushed to; a tag is a commit that was green
when it was named — arriving during a red `main` should not be anyone's first impression.
`BOUGH_REF=main` installs the tip instead, and `BOUGH_REF=v0.2.0` a specific release.

Note that **`bough update` does not hold that pin** — it follows `main`. Installing and
updating answer different questions: a fresh install wants a known-good starting point,
while a checkout that runs `update` is asking to move. Tagged releases are what Homebrew
ships. Set `BOUGH_REF=v0.2.0` in `~/.bough/env` to hold a release across updates.

**Optionally install `bun`.** Programs run under `bun` when it is on PATH and `node`
otherwise. Setup installs only `node`, so the default install takes the fallback path,
`Bun.file`, `Bun.$` and faster process starts come with `bun`.

## Keys

```bash
$EDITOR ~/.bough/env
```

```
ANTHROPIC_API_KEY=sk-ant-…
OPENAI_API_KEY=…          # optional
OPENROUTER_API_KEY=…      # optional
CLOUDFLARE_API_TOKEN=…    # optional; CLOUDFLARE_API_KEY works too
CLOUDFLARE_ACCOUNT_ID=…   # required WITH the Cloudflare token, not instead of it
CEREBRAS_API_KEY=…        # optional; cerebras: entries in the picker
```

Cloudflare is the one provider that needs two values. Discovery asks for the token and
the account id together and returns nothing at all when either is missing, so a token on
its own reads as "no Cloudflare models" rather than as an error.

Models route to a provider by their id prefix, and the model picker (`^o`) lists what
the server's keys can actually reach, not a compiled-in catalog. At least one key is
required for a turn to run.

Two tiers: the **frontier** model runs turns, and a **cheap** model handles titles,
ghost text and activity blurbs. The cheap tier fails silently when it cannot reach a
model, so a missing optional key costs you polish, not function.

## Run

```bash
bough start     # background service: starts at login, restarts on crash
bough           # the TUI (auto-starts the server if it is down)
```

The service manager is the only platform-specific piece: launchd on macOS, a systemd
**user** unit on Linux, a plain background process where there is no user systemd
(containers, WSL1).

## Update

```bash
bough update              # a script install: the tip of main, rebuild, restart
brew upgrade bough        # a Homebrew install
```

Uncommitted changes in the clone are carried across as a patch, not stashed. `BOUGH_REF`
holds a release instead: `BOUGH_REF=v0.2.0`, in `~/.bough/env` to make it stick.

`update` checks out first and then builds, and a build failure is fatal — a `main` that
does not compile aborts the update and leaves the previous binary running.

There is no file watcher. If you are editing bough's own source, changes land only on
an explicit `bough restart`.

## Uninstall

```bash
bough kill                 # stop the service and keep it stopped across logins
rm ~/.local/bin/bough
rm -rf ~/bough             # the clone
rm -rf ~/.bough            # data: sessions, history, artifacts, config
```

Homebrew: `bough kill`, then `brew uninstall bough` in place of the two middle lines. The
data root is yours either way; `brew uninstall` does not touch `~/.bough`.

Removing bough does not touch any repository it has worked in. Your code and its git
history are yours and were never copied anywhere, which is what "in place" means.

There is nothing to prune, either: bough has no archive, hide or purge action and no
columns behind one. A conversation's visibility is derived from what it *is*: a subagent
or workflow branch collapses under the turn that spawned it, so nothing accumulates in a
list waiting to be tidied away.

## Running a local checkout

Contributors: `make dev` runs *this* checkout, TUI and server together, on its own
profile, so it never touches the install at `~/.bough:4321`.

```bash
make dev          # build, start the dev server if it is down, open the TUI
make dev-server   # the server alone, in the foreground
make dev-logs     # tail its log
make dev-stop     # stop it; leaves the real install alone
```

The profile is `.dev/` in the checkout (gitignored, and stable, so dev sessions survive
between runs) on port 4322. Override with `DEV_HOME` and `DEV_PORT`.

Any `BOUGH_HOME` other than `~/.bough` is a profile, and the same rules apply to it:
commands that need a server start one **detached**, never as a login service, because the
launchd/systemd unit belongs to the default profile alone. `bough kill` on a profile stops
that profile's listener and nothing else.
