`app/` builds SQL in a few places by interpolating a value straight into the query
string. Those are the injection risks. Audit the tree and write the findings to
`audit.txt` in the workspace root.

## What counts as a finding

A call to `db.execute(...)` whose **first argument is built from a value** rather
than passed as a parameter — an f-string, `+` concatenation, `%` formatting, or
`.format(...)`.

These are **not** findings:

- `db.execute("... ?", (value,))` — parameterised, which is the correct form
- `log.execute(...)` — a different object with a same-named method
- any occurrence inside a comment or docstring

## The report format

`audit.txt` must contain exactly this and nothing else:

    total: <number of findings>
    <path>:<line>
    <path>:<line>
    ...

One finding per line after the total. `<path>` is relative to the workspace and
starts with `app/`; `<line>` is the 1-based line of the `db.execute` call. Findings are ordered by path, then by line number **numerically** (so `:7` comes
before `:14`). No heading, no bullets, no blank
lines, and a trailing newline at the end.

This is an audit, not a fix: **do not modify any file under `app/`.**
