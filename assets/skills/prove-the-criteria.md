---
name: prove-the-criteria
description: Every behavior the task states is a test you must write and run, especially the failure and interrupt scenarios.
triggers:
  - function
  - implement
  - cancel
  - interrupt
  - cleanup
  - test
  - handle
---
Before finishing an implementation task, turn EVERY stated behavior into an executable check and
run it. The sentences that sound like asides are the requirements that fail: "sometimes I
cancel/kill/restart X and I want Y to still happen" is not color, it is the acceptance test the
user cares most about.

- Write a small driver that uses your artifact exactly the way the task's own examples do
  (import it, call it, run it), covering the happy path AND every stated edge: concurrency
  limits, timeouts, interrupts, partial failure.
- Signals and interrupts cannot be tested in-process: drive your artifact as a SUBPROCESS and
  send the real signal (`proc.send_signal(signal.SIGINT)` after a short sleep), then assert the
  described outcome on its output. A behavior you never triggered is a behavior you never tested.
- When such a test fails, fix the artifact, not the test, and re-run until every stated behavior
  passes. Only then apply the finish-state rule (remove the driver from the deliverable's
  directory: run it from scratch space).

A test that cannot FAIL a plausible-but-wrong implementation proves nothing. Make each probe as
strong as the strongest plausible reading of the words:
- give the stated behaviors their most demanding realistic shape — cleanup code is itself async
  (an await inside the finally) and slow, not a synchronous append; work is long enough that the
  interrupt genuinely lands mid-flight;
- COMBINE the stated edges instead of testing them one at a time: the interrupt must also arrive
  while more work is queued than running (crossing the concurrency boundary), because queue
  handling under cancellation is where implementations differ;
- after your implementation passes, break it on purpose (comment out the part you believe
  matters) and confirm the test goes red — a probe that stays green against a sabotaged artifact
  is a broken probe, and this check costs one minute.
