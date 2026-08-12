# The program environment

The model has exactly two tools: `run_steps(code)` and `stop`. Everything bough can do,
it does by writing one JavaScript program per round and running it.

Control flow lives in the program — loops, branching, error handling, composition —
rather than in a chain of round-trips. A round is one program covering inspect → change
→ verify, not five tool calls.

Programs run in a fresh JS process per round (`bun` if installed, else `node`) with the
server's full authority. The sidecar exists to give the program a clean global scope and
a cancellable lifetime. **It is not a container.** See the warning in the README.

## What is in scope

Eighteen host functions are pre-injected globals — call them directly, never redeclare
one (a pre-flight check fails the program if you do). They are not tools; the model
cannot call them the way it calls `run_steps`.

The program also has the full JS runtime at the user's own permission level:
`node:*` builtins, `Bun.file`, `process.env`, sockets, and `await import("<npm package>")`.

**One absolute exception: every shell command goes through `bash`, `sh` or `bashBg`.**
`child_process.execSync`, `Bun.$` and `Bun.spawn(["sh", "-c", …])` throw. Not for safety
— a command that does not pass through a host function is never recorded, and the
command history is the only memory bough keeps of what has been run in a project.
Spawning a binary directly (`Bun.spawn(["bun", "x.ts"])`) is still allowed when a pipe
or stdin is needed.

## The eighteen

### Shell

| | |
|---|---|
| `await bash(cmd, tags)` | One command in the workspace. Returns **a string** — combined output. Carries the turn's interrupt. `tags` is required. |
| `await sh(...cmds)` | The same shell, concurrently. Returns **objects** — `[{code, out}, …]` in order. Never throws on non-zero; the code is data. |
| `await bashBg(name, cmd)` | Background shell that outlives the turn. Returns `{id, name, pid}`. `name` is required and is what the user sees. |
| `await bashOutput(id)` | Output since the last call, plus a `[running]` / `[exited]` line. Safe to call while running. |
| `await bashWait(id)` | Block until the job finishes. |
| `await bashKill(id)` | SIGTERM the job. |

Mixing `bash` and `sh` return shapes is the single most common way a round dies:
`bash()` gives you the output string itself, so `.out` on it is `undefined`.

A `bash()` still running after ~60s **auto-backgrounds**. It is not killed — the call
returns `…moved to background as bg_N`, the command keeps running, and a note arrives
when it exits. Sleep/poll loops are therefore never necessary.

Output over ~20k chars is **saved to a file**: you get the first and last 5k plus a
marker naming the path. Nothing is lost, and re-running the command to see the middle is
always the wrong move.

That file lands in the session's scratch directory, which every command also gets as
**`$BOUGH_SCRATCH`** — somewhere to put a temp file without choosing a path or littering
the checkout. It is per-session and swept, so nothing there is worth keeping.

`tags` are how commands become recallable across sessions — see [tags.md](tags.md).

### Files

| | |
|---|---|
| `await view(path)` | The file as a `[path#TAG]` header plus numbered `N:text` lines. |
| `await patch(input)` | Hash-anchored line edits against that TAG. Echoes each file's new tag. |
| `await write(path, content)` | New files and wholesale rewrites. Echoes the new tag. |

That is the entire editing surface — there is no `read()` and no `edit()`. Raw content
comes from the runtime or `bash`.

**`patch` names lines instead of quoting them**, so code being edited never has to
survive the model's own string escaping — backticks and `${…}` in the target file cannot
corrupt the match. The TAG pins the version that was read: if the file moved on but the
patched lines are untouched, the edit rebases; if those lines *were* touched, it reports
a conflict naming the range. With subagents sharing one checkout, that is the primary
safeguard against silent clobbering, and a conflict is information rather than a
retryable hiccup.

Paths passed to the raw runtime must be absolute — the program's working directory is
not the workspace. The file verbs and `bash` resolve relative paths against the
workspace; `Bun.file("notes.txt")` does not.

### Delegation

`agent` · `spawn` · `join` · `adopt` · `workflow.*` — see [delegation.md](delegation.md).

### Session

| | |
|---|---|
| `await ask(question, {options?})` | Park the program and ask the human. Returns their answer. Throws a catchable "user declined" if dismissed. |
| `state.get/set/list/delete` | Durable KV for this line of work. Any JSON value, 16KB per value and 200 keys, scoped to the lineage root — so forks, compactions and subagents share one store. |
| `schedule.list/add/enable/disable/remove` | Recurring runs. `spec` is `every:<N><m\|h\|d>` or `daily@HH:MM` local time. Each firing opens a fresh session and reports back as a system note. |
| `await artifact(name, content)` | Publish a file for browser viewing; returns `{url, href}`. Artifacts live outside the workspace, so publishing never pollutes the diff under review. Each carries a comment layer whose batches arrive back as messages. |

## The authoritative text

Every host function above is documented to the model in
[`crates/bough-core/src/prompt/sections/`](../crates/bough-core/src/prompt/sections/) —
one file per capability, `include_str!`-ed into the system prompt, and fatal at boot if
missing. Those files are the contract: they are what the model actually reads, they carry
the failure modes and the worked examples, and when this page disagrees with them, they
are right.

A host function exists only when a prompt section grants it.
