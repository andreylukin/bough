Extend the `tasks.py` CLI in this repo with a priority system. Implement ALL of the following, keeping the existing `add`/`list` behavior working for tasks created without an explicit priority:

1. `add` takes an optional priority flag: `add <text> -p <N>` (also spelled `--priority <N>`), where N is an integer 1..5 and 1 means highest priority. When the flag is omitted the priority defaults to 3. Persist it as the task's `priority`. If N is outside 1..5, print an error to stderr and exit with code 2 (do not create the task).

2. New command `done <id>`: mark that task done and print `done #<id>`. If no task has that id, print `no task #<id>` to stderr and exit 1.

3. `list` now prints tasks sorted by priority ascending (priority 1 first), with ties broken by id ascending. Keep the existing per-line format exactly: `#<id> [ ] <text>`, with `[x]` instead of `[ ]` when the task is done.

4. New command `top`: print just the text of the highest-priority PENDING (not done) task — the lowest priority number, ties broken by the lowest id. If there are no pending tasks, print nothing and exit 0.

The store format (`tasks.json`) and the interpreter are up to you, as long as the observable command behavior above holds. Do not add third-party dependencies — Python 3 stdlib only.
