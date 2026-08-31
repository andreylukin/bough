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

Where the task is SILENT, be literal, not creative. Whoever consumes the result (a script, a
grader, a colleague) will look exactly where the words point, and an unrequested improvement is
indistinguishable from a missing deliverable. Concretely:

- NEVER create a directory the task did not name. No `reports/`, `output/`, `build/` for
  tidiness: organizing is an unrequested improvement, and it hides your deliverable from
  consumers that read the stated directory non-recursively.
- An artifact named without a path (`incident_<ip>_<ts>.txt`, `report.txt`) is created DIRECTLY
  in the task's primary directory — the one holding its inputs and deliverables — as an absolute
  path, so it lands there whatever the caller's cwd is.
- Final check, per artifact the task names: write down the exact absolute path a maximally
  literal reading implies, and confirm the file is at THAT path (`ls` it). If your path contains
  a directory the task never mentioned, you put it in the wrong place.
