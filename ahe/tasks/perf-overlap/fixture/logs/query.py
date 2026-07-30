"""Window queries over the index."""

from collections import Counter
from typing import List, Tuple

from .index import Index
from .model import Event


def overlapping(index: Index, t0: int, t1: int) -> List[Event]:
    """Every event intersecting the window, in index order."""
    hits = []
    for event in index.all_events():
        if event.start <= t1 and t0 <= event.end:
            hits.append(event)
    return hits


def top_tags(index: Index, t0: int, t1: int, k: int) -> List[Tuple[str, int]]:
    """The k most frequent tags in the window, most frequent first."""
    counts = Counter()
    for event in overlapping(index, t0, t1):
        for tag in event.tags:
            counts[tag] += 1
    ranked = sorted(counts.items(), key=lambda pair: (-pair[1], pair[0]))
    return ranked[:k]
