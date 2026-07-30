"""The index.

`build()` is called once, before any query. Whatever a query should not have to
recompute belongs here.
"""

from typing import List

from .model import Event


class Index:
    def __init__(self, events: List[Event]):
        self.events = list(events)
        self._built = False

    def build(self) -> "Index":
        """Prepare whatever the queries need."""
        # Sorting the corpus once, so callers get a stable order.
        self.events.sort(key=lambda e: e.key)
        self._built = True
        return self

    def all_events(self) -> List[Event]:
        return self.events
