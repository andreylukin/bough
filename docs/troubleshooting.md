# Troubleshooting

Start here:

```bash
bough status     # is it up, on what port, since when
bough logs       # server logs
bough --version
```

## It will not start

**Port already in use.** Another instance, or something else on 4321. `BOUGH_PORT=4322
bough start`, or `bough kill` first.

**`bough: command not found`** after install. `~/.local/bin` is not on your PATH. Add it,
or call `~/.local/bin/bough` directly.

**The service does not come back at login.** launchd on macOS and a systemd *user* unit
on Linux. Where there is no user systemd — containers, WSL1 — it falls back to a plain
background process, which does not survive a logout. `bough start` again.

## A turn does nothing, or errors immediately

**No key.** At least one provider key must be in `~/.bough/env`. `bough logs` says which
provider was tried.

**The model id does not match a provider.** Models route by id prefix. The picker (`^o`)
lists what your keys can actually reach; anything else will not resolve.

**"neither bun nor node on PATH".** Programs need a JS runtime. Setup installs `node`;
if it is missing, no program can run. This is logged at startup as a warning, not an
error, so check `bough logs` if turns fail with nothing else to show.

## The model wrote a program that failed

Unfold the step (`^e`) — you get the actual program and its actual output. That is the
first place to look, and usually the last.

Two failures are common enough to name:

**`undefined is not an object` with a stack pointing into the harness.** `bash()` returns
the output *string*; `sh()` returns `[{code, out}]`. Calling `.out` on a `bash()` result
is the single most common way a round dies, and the stack makes it look like bough broke.

**A patch conflict.** The file changed underneath the version that was viewed, on exactly
the lines being edited. This is information, not a hiccup — it means something else
touched that range, usually a sibling subagent. The fix is to re-view and redo the edit,
never to retry the same patch.

## Output went missing

It did not. Output over ~20k chars is written to a file under `~/.bough/scratch/`, and
the marker in the transcript names the path, the size, and what to run next. Read the
file — re-running the command to see the middle is always wrong.

## It does not remember anything

`bough tags` exits `1` when there is no command memory for this repo yet, which is
different from an error. Memory is per repo identity — the git origin URL, else the path
— so a fresh clone with a different remote starts empty.

Semantic recall (`bough tags similar`) needs the optional vector layer. Without
`sqlite-vec` and `sqlite-lembed` it is absent, and everything else still works —
`bough tags show` and `bough tags sql` are the keyword paths and have no such dependency.
`BOUGH_NO_EMBED=1` turns the layer off deliberately.

## Something is wrong with my repo

bough works in place, with no copy and no overlay, so everything it did is in `git diff`
and `git status`. Revert per path from the Changes rail (`^d`), or with your own git.
There is no hidden state to reconcile — that is the point of working in place.

## Still stuck

- [Discussions](https://github.com/andreylukin/bough/discussions) for questions
- [Issues](https://github.com/andreylukin/bough/issues/new/choose) for bugs — the exact
  keystrokes matter, since this is a TUI
- [SECURITY.md](../.github/SECURITY.md) for anything security-related, never the public
  tracker
