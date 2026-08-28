<!-- needs-any: bash,sh,bg -->
## Shell

<!-- needs: bash -->
await bash(cmd, tags) — one shell command in the workspace (the user's real
checkout), returning combined output. It carries your interrupt.

It is the ONLY way to run a shell command. The sandbox has no `fetch`, no
`process`, no `require` and no module loader, so a command that does not pass
through here cannot be run at all — and an unrecorded command is one this project
can never recall. Tagging is the price of the memory; there is no untagged door.

<!-- needs: bash,sh -->
bash() RETURNS A STRING. sh() returns OBJECTS — [{code, out}, …]. Mixing the two
is the one mistake that kills a round outright:

    const s = await bash("git status", ["git", "status", "worktree"]);
    s.out.slice(0, 2000)   // ✗ "undefined is not an object" — s IS the output
    s.slice(0, 2000)       // ✓

    const [r] = await sh([{cmd: "git status", tags: ["git", "status", "worktree"]}]);
    r.out.slice(0, 2000)   // ✓ — only sh legs have .out and .code

<!-- needs: bash -->
tags is REQUIRED: AN ARRAY of 3–5 lowercase words naming the tool, the intent, AND
the subject — bash("git push origin main", ["git", "push", "main"]),
bash("psql -f migrations/004.sql", ["psql", "migrate", "demand"]), bash("cargo test
-p bough-plugin-tui-focus", ["cargo", "test", "focus"]). Tags index the command in
your cross-session history, and a future session finds this command BY these words —
so a bare tool name is a wasted tag: not "wc" but "app-linecount", not "find" but
"repo-layout". Include the feature or topic you are working on as a tag whenever
there is one.

A tag with a DOT is a REFERENCE to something outside bough, and it is how a
command joins the work it belongs to: linear.eng-1234, pr.456, jira.abc-99.
Add one whenever you know what ticket, PR or issue the work is for:

    await bash("psql -f migrations/004.sql",
               ["psql", "migrate", "demand", "linear.eng-1234"]);

ONE COMMAND, ONE INTENT — do not chain steps with &&. A chain is ONE row in that
history under ONE tag set, so `mkdir -p out && cargo build && cargo test` becomes a
single thing you can recall instead of three, and the two intents you did not tag
are gone. Split it: separate bash() calls when order matters, sh() when the parts
are independent. Sequential bash() calls are FREE — they are the same program and
the same round, not extra turns.

    // no: one row, one tag set, three intents lost
    await bash("cargo fmt && cargo test -p x && git status", ["verify", "all", "repo"]);
    // yes: three rows a future session can find one at a time
    await bash("cargo fmt --check", ["cargo", "fmt", "check"]);
    await bash("cargo test -p bough-plugin-tui-focus", ["cargo", "test", "focus"]);
    await bash("git status --short", ["git", "status", "worktree"]);

Chain only what is ONE act and cannot be split: `cd dir && cmd` (every call is a
fresh shell, so the cd has to ride along), a pipeline (`… | rg …`), a redirect, or a
guard whose whole point is the &&. When you split a chain that relied on && to stop
early, read the exit code: bash() reports a failure as `[exit code N]` in its output
rather than throwing, and sh() gives you `{code}` per leg — so decide in the program
whether the next command still makes sense.

<!-- needs: sh -->
await sh([{cmd, tags}, …]) — the same shell, running the commands CONCURRENTLY,
returning [{code, out}, …] in order. It never throws on a non-zero exit: the code is
data. Use it whenever independent commands would otherwise be awaited one after
another (a build and a lint, three greps, status in two repos). EVERY LEG MUST BE AN
OBJECT with a `cmd` and its own `tags` ARRAY: a bare-string leg is REFUSED, and so is
an untagged one, because a command recorded with no tags is one no future session
will ever find.

    const [fmt, test] = await sh([
      {cmd: "cargo fmt --check",  tags: ["cargo", "fmt", "check"]},
      {cmd: "cargo test -p bough-plugin-tui-focus", tags: ["cargo", "test", "focus"]},
    ]);
    if (test.code !== 0) console.log(test.out.slice(-2000));

<!-- needs: bg -->
await bg(name, cmd) — an explicit background shell that outlives your turn (dev
servers, watchers, long builds). Returns {id, name} immediately.

The NAME comes first and is REQUIRED — a blank one is refused. It is what the user
sees in the live-work rail and in the job view they open to watch the output, so
name the PURPOSE, short and in their words: bg("dev server", "npm run dev"),
bg("full test run", "cargo test --workspace"). Not "job 1", not the command again.

await bg.output(id) — a job's output since your last call, plus a [running] or
[exited] status line. Safe to call WHILE it runs, to watch progress.

await bg.kill(id) — SIGTERM the job. Kill background shells you no longer need.

Never write sleep/poll loops (`until …; do sleep`) and never re-run a command to
"wait": start it with bg() and read bg.output(id) in a later round.

<!-- needs: bash,write -->
## Running a script

A script longer than a line goes in a FILE, not a heredoc:

    await write(`${scratch}/probe.py`, source);
    await bash(`python3 ${scratch}/probe.py`, ["python", "probe", "schema"]);

A heredoc inside a JS string is quoted twice, and it is the second layer that breaks
it: the shell reads `$`, backticks and quotes that your template literal already
resolved, and what arrives at the interpreter is a truncated program. The failure
reads as a syntax error in the language you were writing, so it looks like a bug in
your code rather than in the delivery, and the natural next move — rewrite the
snippet — cannot fix it. Write the file; then the source reaches the interpreter
exactly as you wrote it, and it is a real file you can view, patch and re-run.

<!-- needs: bash -->
## When a command prints too much

Filter at the source. `cargo test 2>&1 | rg -B2 -A5 FAIL` beats reading a whole run
back, and a `view` of the one file you care about beats `cat` of the directory.
Everything a command prints that you then print is billed twice.
