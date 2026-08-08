# Command reference

`bough` with no arguments opens the TUI. `bough help` lists everything; every subcommand
takes `-h`.

Two families. The **service manager** (`scripts/bough`, a bash wrapper) owns the process
lifecycle; the **binary** owns everything else.

## Service

| | |
|---|---|
| `bough setup` | Fresh clone → running: install dependencies, configure env, start |
| `bough start` | Install and start the background service (auto-starts at login) |
| `bough kill` | Stop the service and keep it stopped across logins |
| `bough restart` | Pick up code changes — there is no file watcher |
| `bough update` | Fast-forward to `origin/main`, rebuild, restart |
| `bough status` | Is it up, on what port, since when |
| `bough logs` | Server logs |
| `bough run` | Run the server in the foreground (what the service executes) |
| `bough purge` | Delete sessions archived more than 30 days ago. Needs the server up |
| `bough --version` | The version (`-V` and `bough version` also work) |

## `bough exec` — headless turn

```
bough exec [-w DIR] [-m MODEL] [--json] [--timeout SECS] [--port N] "prompt"
```

Creates a session, streams the assistant's text to stdout, exits. The prompt may also
come on stdin. `--timeout` is wall clock for the whole turn, default 900s. `--json` gives
one JSON envelope per line.

**Exit codes:** `0` completed · `1` the turn errored · `2` usage or connection problem.
That split is the contract worth scripting against — a non-zero from a *failed turn* is
distinguishable from a non-zero from a *broken invocation*.

## `bough acp` — Agent Client Protocol

```
bough acp
```

Speaks ACP on stdin/stdout so a client like Zed can drive bough sessions and receive
streaming updates. It is a **client of the bough server, not a second server** — start
`bough start` first. stdout carries the protocol and nothing else; diagnostics go to
stderr.

## `bough tags` — the command memory

```
bough tags                  # this project's tag vocabulary, as the model is primed with it
bough tags show TAG         # commands recorded under TAG, newest first
bough tags stats            # coverage and vocabulary per day
bough tags sql "SELECT …"   # read-only SELECT over the memory and the transcripts
bough tags similar "text"   # semantic recall, where the local vector layer exists
```

`--repo R` scopes to a repo identity, `--all` spans every repo, `--program` prints the
program each command ran in, `--limit` / `--days` / `--json` do the obvious.

Exit `1` means no command memory yet, which is different from an error. Full mechanism:
[tags.md](tags.md).

## `bough patterns` — read a big log

```
bough patterns [--llm|--json|--human] [--top N] [--threshold F] [FILE]
kubectl logs … | bough patterns --llm
```

Compresses a log into the distinct statements it is made of: templates with counts, typed
variable statistics, flagged anomalies, problems first. Reads stdin when `FILE` is absent.
Defaults to `--human` on a terminal and `--llm` otherwise.

Raise `--threshold` if distinct statements are being merged; lower it if one statement is
splitting into near-duplicates.

## `bough mcp` — servers and grants

```
bough mcp                            # every server's state
bough mcp call SERVER TOOL '{"a":1}' # how a program invokes a tool
bough mcp add|remove|list|test|doctor
bough mcp auth|logout|grant|revoke
```

`bough sync-mcp` adopts Claude Code's configured servers.

## `bough hooks`

```
bough hooks     # what is installed, what is on, listener counts
```

See [extending.md](extending.md).
