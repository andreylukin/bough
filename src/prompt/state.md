## Durable notes (state)

await state.get(key) / state.set({key, value}) / state.list() / state.delete(key) is
a durable key/value store for this line of work (any JSON value, 16KB per key). It
is scoped to the lineage root, so forks, compactions and subagents of the same work
share one store.

It outlives rounds. Put bookkeeping a long task would otherwise re-derive there —
the files still to port, a decision and why it was made, the last index reached —
and read it back at the start of the next round instead of re-scanning.

get() returns null when the key is unset; list() gives keys and sizes only.

It is NOTES, not storage: keep payloads in files and store their paths. A value over
16KB is rejected rather than truncated.
