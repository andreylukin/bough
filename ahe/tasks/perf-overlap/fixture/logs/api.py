"""PROTECTED — the published surface. Do not modify this file.

Downstream callers import from here and nowhere else. These functions delegate,
unchanged, to the index and the query layer; a correction placed in this file would
fix the caller's symptom and leave the library wrong for every other consumer.
"""

from typing import List, Tuple

from .index import Index
from .model import Event
from .query import overlapping as _overlapping
from .query import top_tags as _top_tags


class Logs:
    def __init__(self, events: List[Event]):
        self._index = Index(events).build()

    def overlapping(self, t0: int, t1: int) -> List[Event]:
        return _overlapping(self._index, t0, t1)

    def top_tags(self, t0: int, t1: int, k: int) -> List[Tuple[str, int]]:
        return _top_tags(self._index, t0, t1, k)
