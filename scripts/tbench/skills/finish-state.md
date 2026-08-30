---
name: finish-state
description: Converge to exactly the end state the task describes; verification scratch is yours to remove.
triggers:
  - write
  - file
  - create
  - single
  - output
  - print
---
The task's description of the end state is a CONTRACT, and it constrains more than the artifact:
"a single file in DIR" constrains the whole directory; "prints exactly X" constrains every byte;
a named path means that path and no sibling debris.

- Verify as hard as you can, but do not let verification POLLUTE the described state: compile and
  run in a scratch directory (`d=$(mktemp -d)` and copy the artifact there), or remove what your
  checks created before you finish (binaries, `__pycache__`, logs, helper scripts, test files).
- The line to draw: state the task ASKS to exist (an installed package, a running service, the
  deliverable file) stays; state only YOUR verification created goes.
- Last step before finishing, always: list the task's target locations (`ls -la` each one) and
  confirm they hold exactly what the task described, nothing extra, then re-run the task's own
  acceptance commands one final time from that clean state.
