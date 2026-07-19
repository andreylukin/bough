The record-parsing snippet is copy-pasted into receiving.py, shipping.py and audit.py, and the copies have drifted (one strips fields, one handles `#` comments, one does neither). Consolidate: create `lib/parsing.py` containing a single function `parse_record(line)`, make all three modules use it, and delete the local copies of the snippet.

`parse_record(line)` — this exact behavior is canonical, everywhere:

1. Everything from the first `#` on is a comment: remove it.
2. If nothing but whitespace remains, return `None` (callers skip the line).
3. Otherwise split on `|`; anything other than exactly 3 fields raises `ValueError(f"bad record: {line!r}")` with the original, unmodified line.
4. Strip whitespace from each field; convert the middle field with `int()`. Return the tuple `(sku, qty, loc)`.

Where a drifted copy disagrees with this spec, the spec wins. Otherwise the three modules' observable behavior must be preserved: `python3 -m unittest` must pass, and test_warehouse.py must not be changed.
