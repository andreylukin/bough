"""The event type. Stable; the index and the queries are what interpret it."""

from dataclasses import dataclass, field
from typing import List


@dataclass(frozen=True)
class Event:
    id: int
    start: int
    end: int
    tags: List[str] = field(default_factory=list)

    @property
    def key(self):
        """Ordering: by start, then by id."""
        return (self.start, self.id)
