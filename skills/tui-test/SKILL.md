---
name: tui-test
description: Drive bough's own TUI (or any TUI program) in a headless PTY inside the sandbox — spawn an isolated server, send keys, screenshot, assert
---

# Testing the TUI from inside the sandbox

You can run and drive real terminal programs — including bough's own TUI — with
the `shell-use` CLI (a PTY daemon + stateless client, on PATH). The sandbox
supports this, with two rules baked into the seatbelt profile:

- The **live** bough server (`localhost:4321`) is unreachable by design — you
  can never drive the control plane that gates your own turn. Test against an
  isolated server you start yourself (below).
- The operator's own shell-use daemon (`~/.shell-use`) is unreachable by
  design — always start your own daemon under a scratch `$HOME`.

## 1. Daemon under a scratch HOME

Pick a SHORT path (unix sockets cap at ~104 bytes — the session scratchpad
path is too long):

```bash
export SUHOME=/tmp/su-$$ && mkdir -p "$SUHOME"
su() { HOME="$SUHOME" shell-use --session t "$@"; }
```

Every shell-use call goes through `su` so the daemon, socket, and recordings
stay under `$SUHOME`.

## 2. Isolated bough server (only for testing bough's TUI)

From the workspace (the bough repo), on a port that isn't 4321:

```bash
BOUGH_CLAWPATROL=0 BOUGH_PORT=4390 BOUGH_DB=/tmp/su-$$-db/bough.db \
  deno run --allow-net --allow-env --allow-read --allow-write --allow-ffi \
  --allow-sys --allow-run src/server/main.ts &
# poll until: curl -s http://127.0.0.1:4390/skills succeeds
```

Fresh empty DB, no egress proxy — good for UI/UX testing (composer, pickers,
panels, keys, rendering). Real turns won't run (no LLM egress from the
sandbox), so don't test turn execution this way.

## 3. Run the TUI in the PTY

`deno task tui` needs the real HOME (deno cache) and the test port:

```bash
su run --cwd "$PWD" --env HOME="$HOME" --env BOUGH_PORT=4390 \
  --cols 100 --rows 30 deno task tui
sleep 3   # let it paint
```

## 4. Drive and assert

```bash
su screenshot                      # rendered text to stdout
su type "/"                        # type literal text (no enter)
su press Enter Escape Ctrl+P       # named keys / chords
su wait idle                       # screen stopped repainting
su expect text "handoff" --no-strict   # exit 1 if not visible
su mouse click 10 5                # SGR mouse events work too
```

`su screenshot out.svg` renders a full-color SVG. `shell-use agent-context`
(through `su`) dumps the full machine-readable command surface.

## 5. Clean up

```bash
su close
HOME="$SUHOME" shell-use daemon stop
kill %1   # the isolated server
```

Works the same for any other TUI/CLI (vim, htop, a curses app you're
building): skip step 2 and just `su run` the program.
