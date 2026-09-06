---
name: current-work
description: What I am working on right now, from the memory graph — open PRs, tickets, threads, what awaits me, what changed, and what bough remembers about each. Invoke as /current-work.
---

# current-work

Tell me where my work stands, from what bough has collected and remembered.
Read the graph; do not go to GitHub, Linear, Slack or Notion yourself — the
collectors already did, and the links are in the graph.

## Steps

Do all of this in one program.

1. Freshness. `tools.graph.world()` returns the world around me:
   "Waiting on me" and "Mine, open", one line per thing with its key,
   title, [status], who it awaits, a summary, and its link, ending in a
   `(collected …)` stamp. If the stamp is older than 30 minutes, run
   `tools.bash("bough collect || ~/repos/bough/go/bough collect")` first
   (about a minute) and read the world again.

2. Group by ticket. For every `ticket:` line, `tools.graph.neighbors(key)`
   finds the PRs that implement it (`implements`), the threads and pages
   about it (`discusses`, `documents`) and the sessions that touched it
   (`touches`). A PR or thread that belongs to a ticket is reported under
   the ticket, not on its own.

3. What changed. For each open PR and ticket, `tools.graph.timeline(key)`
   lists its edges newest first, closed windows included. Report a
   `has_state` change in the last two days ("moved to code_review
   yesterday", "merged this morning") and an `awaits` edge that closed
   ("Bradley reviewed").

4. What I remember. `tools.graph.search(<ticket or PR title>, 5)` returns
   the auto-memory claims (author `cheap`) and my own notes (`session`)
   around it. Quote the one or two that say where I left off or what I
   decided. Skip claims that only restate the title.

5. Loose ends. Anything in "Waiting on me" that is not under a ticket goes
   in its own group at the top: reviews requested of me, threads where
   someone else had the last word.

Print compactly. `neighbors` and `timeline` return full edge objects;
never print them raw. Map each to one line, e.g.
`${e.src.kind}:${e.src.key} ${e.rel} ${e.dst.kind}:${e.dst.key} ${e.dst.status} ${e.valid_to ? "closed" : ""}`,
and for search hits the claim text. The whole program's output should fit
in a few hundred lines.

## Output

Markdown, short, links inline. Order: waiting on me, then tickets by most
recently updated, then loose PRs, then a one-line "collected <time>" tail.

```
## Waiting on me
- [uni-nes#7566](url) Make NASED backup alerts deterministic — ci failing, 2 unresolved bot threads

## NME-1789 Check public routes in CI — in_progress, due Mon
- [uni-cas-manager#712](url) ci failing, review required
- [uni-orb#173](url) awaits Bradley since yesterday
- remembered: the CI check renders dev and prod VirtualServices (2026-09-04)
- last session: "check public routes" 2 days ago

## Loose PRs
- [uni-notion#16](url) Make the atlas multi-source — ci pending, nobody requested
```

One line per thing. No status a link already says. If the graph is empty,
say so and suggest `bough collect install`.
