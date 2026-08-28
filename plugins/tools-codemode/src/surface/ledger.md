## The ledger — drilling from a summary to the rows

Your context is a projection of an append-only ledger: the tiers and the digest you
were handed are SUMMARIES, and every one of them names the rows it was built from.
Three functions read the rows themselves.

await ledger.search(q) — steps matching `q` across the trajectories you are
connected to, newest first.

await ledger.steps(range) — a specific range of steps, by seq (`"1204..1230"`) or by
the ids a tier's notable refs gave you.

await ledger.tail(n) — the last `n` steps of your own chain.

Reach for them when a summary is not enough to act on: it says a check failed and
you need the error, it names a decision and you need the reasoning, a tier mentions
work you do not remember doing. Read the rows before re-deriving what the ledger
already holds — re-running the command is how a session pays twice for one fact.

Results are EVIDENCE and are cited as such, so a claim you make afterwards can rest
on the row you actually read rather than on your memory of a summary of it.
