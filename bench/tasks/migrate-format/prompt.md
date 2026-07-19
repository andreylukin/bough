records.dat and legacy.dat store records in the v1 format: one record per line, `id,name,qty,note` (note is everything after the third comma; there is no escaping, so values can never contain newlines). We are migrating to the v2 tagged format:

- Line 1 is exactly `#recio v2`.
- Each record follows as one `key=value` line per field, in this exact order: `id`, `name`, `qty`, `note`. All values are strings.
- Records are separated by exactly one blank line; no blank line after the last record; the file ends with a newline. A file with no records is just the header line.
- Escaping, applied to values on write and reversed on read: backslash becomes `\\`, newline (LF) becomes `\n`, equals sign becomes `\=`. Keys are never escaped.
- On read: the key is everything before the first `=`; keys other than the four above are ignored; a field missing from a record reads as the empty string; fields may appear in any order.

Deliverables:

1. `migrate.py`: `python3 migrate.py FILE` rewrites FILE in place from v1 to v2. Running it on a file that is already v2 must leave the bytes unchanged (idempotent).
2. `recio/reader.py`: `read_records(path)` auto-detects the format by the header line and reads both v2 and old v1 files. v1 reading behavior must not change (test_recio.py stays green and untouched).
3. `recio/writer.py`: `write_records(path, records)` now always writes v2.
4. Migrate records.dat in the repo — it must be v2 in your final state, byte-for-byte:

```
#recio v2
id=r1
name=alpha widget
qty=4
note=fragile

id=r2
name=beta
qty=12
note=size\=XL, ship flat

id=r3
name=gamma
qty=1
note=

id=r4
name=back\\slash
qty=2
note=keep \\n literal
```

5. Leave legacy.dat exactly as it is — an old system still consumes it, and it must still load through `read_records` (and thus `python3 dump.py legacy.dat`).

The round-trip property `read_records(write_records(x)) == x` must hold; `python3 -m unittest checker` runs the provided acceptance checks and must pass. Do not modify checker.py, dump.py, test_recio.py, or legacy.dat. `python3 -m unittest` must stay green.
