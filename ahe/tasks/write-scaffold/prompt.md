Write a new package `dsv/` in the workspace root. Nothing exists yet.

It parses a small delimiter-separated format. Export exactly two names from
`dsv/__init__.py`: `parse` and `ParseError`.

## `parse(text, delimiter=",")` -> list[dict]

The first non-empty line is the header; each later line is a record. Every record
comes back as a dict mapping header name to value.

1. **Quoting.** A field may be wrapped in double quotes. Inside quotes, the
   delimiter and newlines are literal text, and `""` is one literal `"`. Quotes are
   stripped from the value. A quote that opens and never closes raises
   `ParseError`.
2. **Whitespace.** Unquoted fields are stripped of leading and trailing spaces and
   tabs. Quoted fields are not — `" a "` keeps its spaces. Text outside the closing
   quote of a quoted field (e.g. `"a"x`) raises `ParseError`.
3. **Blank lines** — empty or whitespace only — are skipped anywhere, including
   before the header. A line inside a quoted field is not a blank line.
4. **Arity.** A record with more fields than the header raises `ParseError`. A
   record with fewer gets `None` for each missing trailing column.
5. **Types.** A field that looks like an integer (optional `-`, then digits only)
   becomes an `int`. Everything else stays a `str`. A **quoted** field is always a
   `str`, even if it looks numeric. An unquoted empty field is `None`.
6. **Duplicate header names** raise `ParseError`. An empty header name does too.
7. `parse("")`, or text that is entirely blank lines, returns `[]`.

`ParseError` subclasses `Exception`.

The delimiter is always a single character and is never `"`.
