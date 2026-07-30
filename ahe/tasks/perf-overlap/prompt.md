`logs/` answers time-window queries over a stream of session events. It is both
wrong and too slow. Fix it.

## Spec

An event has an integer `id`, a half-open interval `[start, end)`, and a list of
`tags`.

1. **`overlapping(t0, t1)`** returns every event whose interval intersects the
   half-open window `[t0, t1)`. Two half-open intervals intersect when
   `start < t1 and t0 < end` — an event that merely touches the boundary (ends
   exactly at `t0`, or starts exactly at `t1`) does **not** overlap. Results are
   ordered by `start`, then by `id`.

2. **`top_tags(t0, t1, k)`** returns the `k` most frequent tags among the events
   overlapping that window, most frequent first. Ties are broken by **first
   appearance** — the tag whose earliest-overlapping event comes first in the
   ordering from rule 1 wins. Ties are *not* broken alphabetically.

3. **It must be fast.** The grading suite runs several thousand queries against a
   corpus of a few hundred thousand events, with a wall-clock budget. Re-scanning
   the corpus on every query will not fit in it. `Index.build()` is called once,
   before any query, and exists so that queries do not have to.

## Constraints

- `logs/api.py` is the published surface and is **protected**: do not modify it.
- `test_logs.py` is the checked-in test suite. It must still pass.
- The standard library only. No numpy, no new dependencies.
