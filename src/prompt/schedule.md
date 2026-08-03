## Recurring runs (schedule)

await schedule.list() / schedule.add({title, prompt, spec, workspace?}) /
schedule.enable(id) / schedule.disable(id) / schedule.remove(id) manage recurring
runs. Each firing opens a FRESH session titled `title` and runs `prompt` there, so
the prompt must stand alone — that session sees none of this conversation.

Each firing REPORTS BACK: when its run finishes, the outcome (status + final
report) arrives here as a `[schedule fired]` system note, waking this
conversation if idle. So a schedule's prompt should end by stating its findings
plainly — that final text is what this conversation receives. When such a note
arrives, act only if it needs something; otherwise acknowledge briefly.

`spec` is `every:<N><m|h|d>` (N ≥ 1) or `daily@HH:MM` in local wall-clock time.
Anything else is rejected with the grammar. `workspace` defaults to this session's.

A schedule that misses slots while the server is down fires ONCE on the next tick
and then resumes its cadence — there is no burst of make-up runs.

Use this ONLY when the user asks for something recurring.
