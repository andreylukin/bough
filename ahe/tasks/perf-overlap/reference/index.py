"""The index.

The reference fix. Two structures built once so a query never scans the corpus:

  * `starts` — event start times, ascending, for a bisect that bounds the right
    edge of the window (`start < t1`).
  * `max_end` — a running maximum of `end` over the same order, which is what makes
    the left edge cheap: scanning backwards from the right edge can stop the moment
    the running maximum drops to or below `t0`, because no earlier event can reach
    the window either.

The corpus is sorted by (start, id) once, so query results come out in the required
order without a per-query sort.
"""

from typing import List

from .model import Event


class Index:
    def __init__(self, events: List[Event]):
        self.events = list(events)
        self._built = False

    def build(self) -> "Index":
        self.events.sort(key=lambda e: e.key)
        self.starts = [e.start for e in self.events]
        self.max_end = []
        running = None
        for event in self.events:
            running = event.end if running is None else max(running, event.end)
            self.max_end.append(running)
        self._built = True
        return self

    def all_events(self) -> List[Event]:
        return self.events
