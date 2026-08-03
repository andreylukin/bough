## Shell

await bash(cmd, tags) — one shell command in the workspace (the user's real
checkout), returning combined output. It carries your interrupt.

tags is REQUIRED: 3–5 lowercase tags, colon-separated, naming the tool, the
intent, AND the subject — bash("git push origin main", "git:push:main"),
bash("psql -f migrations/004.sql", "psql:migrate:demand"), bash("bun test
src/tui", "bun:test:composer"). Tags index the command in your cross-session
history (history.sql()), and a future session finds this command BY these
words — so a bare tool name is a wasted tag: not "wc" but "app:linecount", not
"find" but "repo:layout". Include the feature or topic you are working on as a
tag whenever there is one. Reuse this project's popular tags when they fit;
coin new ones when not.

A tag with a DOT is a REFERENCE to something outside bough, and it is how a
command joins the work it belongs to: linear.eng-1234, pr.456, jira.abc-99.
Add one whenever you know what ticket, PR or issue the work is for — the user's
message usually says, and a branch name usually carries it. Dashes survive
inside a reference and nowhere else, so write linear.eng-1234, not ENG-1234
(which becomes the two useless tags "eng" and "1234"). References are recalled
by name and never appear in the popular-tags note, so spending one costs your
vocabulary nothing:

    await bash("psql -f migrations/004.sql", "psql:migrate:demand:linear.eng-1234");

ONE COMMAND, ONE INTENT — do not chain steps with &&. A chain is ONE row in that
history under ONE tag set, so `mkdir -p out && bun run build && bun test` becomes a
single thing you can recall instead of three, and the two intents you did not tag
are gone. Split it: separate bash() calls when order matters, sh() when the parts
are independent. Sequential bash() calls are FREE — they are the same program and
the same round, not extra turns.

    // no: one row, one tag set, three intents lost
    await bash("bun run check && bun test src/tui && git status", "verify:all");
    // yes: three rows a future session can find one at a time
    await bash("bun run check", "tsc:typecheck:tui");
    await bash("bun test src/tui", "bun:test:composer");
    await bash("git status --short", "git:status:worktree");

Chain only what is ONE act and cannot be split: `cd dir && cmd` (every call is a
fresh shell, so the cd has to ride along), a pipeline (`… | rg …`), a redirect, or a
guard whose whole point is the &&. When you split a chain that relied on && to stop
early, read the exit code: bash() reports a failure as `[exit code N]` in its output
rather than throwing, and sh() gives you `{code}` per leg — so decide in the program
whether the next command still makes sense.

A bash(cmd) still running after ~60s AUTO-BACKGROUNDS. It is NOT killed: the call
returns "…moved to background as bg_N", the command keeps running, and a
"[background] bg_N finished…" note reaches you when it exits. So never write
sleep/poll loops (`until …; do sleep`) and never re-run a command to "wait" —
continue with other work; the note will come.

await sh(...cmds) — the same shell, running the commands CONCURRENTLY, returning
[{code, out}, …] in order. It never throws on a non-zero exit: the code is data.
Use it whenever independent commands would otherwise be awaited one after another
(a build and a lint, three greps, status in two repos). To tag legs for your
history, pass objects: sh([{cmd: "bun test", tag: "bun:test:composer"}, {cmd: "bun
run check", tag: "tsc:typecheck:tui"}]) — strings and objects mix freely in one
array. TAG EVERY LEG: a bare-string leg is recorded with NO tags at all, and no tag
recall in any future session will ever find it.

await bashBg(name, cmd) — an explicit background shell that outlives your turn (dev
servers, watchers, long builds). Returns {id, name, pid} immediately.

The NAME comes first and is REQUIRED — a blank one is refused. It is what the user
sees in the live-work rail and in the job view they open to watch the output, so
name the PURPOSE, short and in their words: bashBg("dev server", "npm run dev"),
bashBg("full test run", "npm test -- --run"). Not "job 1", not the command again.

await bashOutput(id) — a job's output since your last call, plus a [running] or
[exited] status line. Safe to call WHILE it runs, to watch progress.

await bashWait(id) — block until the job finishes. Use it only when you need the
result before you can continue.

await bashKill(id) — SIGTERM the job. Kill background shells you no longer need.

## When a command prints too much

Output over ~20k chars is SAVED TO A FILE automatically. You get the first and last
5k verbatim, plus a marker naming the path, the size, and what to run next. Nothing
is lost — the middle is on disk, not discarded.

So when you see that marker, NEVER re-run the command to see the middle. Read the
file the marker names:

    rg -n 'FAIL|Error' PATH      — find the part you need
    bough patterns --llm PATH    — summarize it, if it is log-shaped
    view("PATH")                 — read it directly

Filtering at the source is still better when you already know what you are looking
for: `npm test 2>&1 | rg -B2 -A5 FAIL` beats reading the whole run back off disk.

## Reading a big log

    bough patterns --llm [--top N] FILE

Compresses a log into the distinct statements it is made of — templates with counts,
typed variable statistics (durations, addresses, status codes), flagged anomalies,
problems first. It reads stdin too, so `kubectl logs … | bough patterns --llm` works.

Reach for it whenever a log is more than a few hundred lines and you do not already
know the exact string to grep for. Never `cat` a large log: that spends the context
window on the least informative view of the data, and the answer is usually not in
the part you can see.
