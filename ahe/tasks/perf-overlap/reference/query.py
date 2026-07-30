"""Window queries over the index.

The reference fix. Three defects:
  * the overlap test was inclusive on both ends; the spec's intervals are half-open
    (`start < t1 and t0 < end`), so a touching boundary is not an overlap
  * ties in `top_tags` were broken alphabetically rather than by first appearance
  * every query scanned the whole corpus; the index now bounds the scan
"""

from bisect import bisect_left
from collections import Counter
from typing import List, Tuple

from .index import Index
from .model import Event


def overlapping(index: Index, t0: int, t1: int) -> List[Event]:
    """Every event intersecting the half-open window, in index order."""
    events = index.all_events()
    # Right edge: the first event with start >= t1 cannot overlap, nor can any after
    # it, since starts are ascending.
    hi = bisect_left(index.starts, t1)
    hits = []
    for i in range(hi - 1, -1, -1):
        # No event at or before i reaches past max_end[i], so once that falls to t0
        # the rest of the prefix is out of the window entirely.
        if index.max_end[i] <= t0:
            break
        event = events[i]
        if t0 < event.end:
            hits.append(event)
    hits.reverse()
    return hits


def top_tags(index: Index, t0: int, t1: int, k: int) -> List[Tuple[str, int]]:
    """The k most frequent tags in the window, ties broken by first appearance."""
    counts = Counter()
    first_seen = {}
    for position, event in enumerate(overlapping(index, t0, t1)):
        for tag in event.tags:
            counts[tag] += 1
            if tag not in first_seen:
                first_seen[tag] = position
    ranked = sorted(counts.items(), key=lambda pair: (-pair[1], first_seen[pair[0]]))
    return ranked[:k]
