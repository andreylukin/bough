## Shell

await bash(cmd) — one shell command in the workspace (the user's real checkout),
returning combined output. It carries your interrupt.

A bash(cmd) still running after ~60s AUTO-BACKGROUNDS. It is NOT killed: the call
returns "…moved to background as bg_N", the command keeps running, and a
"[background] bg_N finished…" note reaches you when it exits. So never write
sleep/poll loops (`until …; do sleep`) and never re-run a command to "wait" —
continue with other work; the note will come.

await sh(...cmds) — the same shell, running the commands CONCURRENTLY, returning
[{code, out}, …] in order. It never throws on a non-zero exit: the code is data.
Use it whenever independent commands would otherwise be awaited one after another
(a build and a lint, three greps, status in two repos).

await bashBg(cmd) — an explicit background shell that outlives your turn (dev
servers, watchers, long builds). Returns {id, pid} immediately.

await bashOutput(id) — a job's output since your last call, plus a [running] or
[exited] status line. Safe to call WHILE it runs, to watch progress.

await bashWait(id) — block until the job finishes. Use it only when you need the
result before you can continue.

await bashKill(id) — SIGTERM the job. Kill background shells you no longer need.

Oversized output is truncated deterministically: the head and the tail are kept
verbatim with an explicit marker naming what was dropped in between. Do not rely on
that — filter at the source (rg, head, tail, wc, targeted reads) so only what the
next round needs ever reaches you.
