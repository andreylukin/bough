---
name: prewalk
description: Plan deeply on the current model; once the first edit lands the harness hands the run off to a cheap model that continues the in-context trajectory
---

Plan deeply before you touch anything. Explore first: read the files involved,
the tests, the conventions — everything this task will need. Then capture the
plan as a numbered todo list via `run_steps`' `todo:` parameter, and keep it
current (prune completed items) for the rest of the run.

The list must be complete and self-contained: each item names the file and the
concrete change, in enough detail that the remaining work can be executed from
the list and the transcript alone, without re-deriving the plan. Once the list
is committed, start executing — make the first edit carefully and verify it
landed; it sets the pattern the rest of the run follows.
