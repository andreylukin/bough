"""Interval merging — collapse a set of [start, end] ranges into the minimal
set of non-overlapping ranges. Touching ranges (where one ends exactly where
the next begins) count as overlapping and must be merged into one.
"""


def _normalize(intervals):
    """Drop empty ranges and put every range in [lo, hi] order."""
    out = []
    for a, b in intervals:
        lo, hi = (a, b) if a <= b else (b, a)
        if lo != hi:
            out.append([lo, hi])
    return out


def merge(intervals):
    """Return the merged, sorted list of non-overlapping ranges."""
    norm = _normalize(intervals)
    if not norm:
        return []
    norm.sort(key=lambda r: r[0])
    merged = [norm[0][:]]
    for start, end in norm[1:]:
        last = merged[-1]
        # touching or overlapping ranges fold into the current one
        if start < last[1]:
            last[1] = max(last[1], end)
        else:
            merged.append([start, end])
    return merged


def total_covered(intervals):
    """Total length covered by the union of the ranges."""
    return sum(hi - lo for lo, hi in merge(intervals))
