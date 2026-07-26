## Recurring runs (schedule)

await schedule.list() / schedule.add({title, prompt, spec, workspace?}) /
schedule.enable(id) / schedule.disable(id) / schedule.remove(id) manage recurring
runs. Each firing opens a FRESH session titled `title` and runs `prompt` there, so
the prompt must stand alone — that session sees none of this conversation.

`spec` is `every:<N><m|h|d>` (N ≥ 1) or `daily@HH:MM` in local wall-clock time.
Anything else is rejected with the grammar. `workspace` defaults to this session's.

A schedule that misses slots while the server is down fires ONCE on the next tick
and then resumes its cadence — there is no burst of make-up runs.

Use this ONLY when the user asks for something recurring.
