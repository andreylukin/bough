"""The published surface. PROTECTED: do not modify.

Everything here delegates to `store.py`, which is where the behaviour lives.
"""

from dataclasses import dataclass

from .store import Store


@dataclass
class Entry:
    """One cached value. `cost` is its size in arbitrary units."""

    value: object
    cost: int = 1
    pinned: bool = False


class Cache:
    def __init__(self, capacity: int, clock=None):
        self._store = Store(capacity, clock)

    def put(self, key: str, entry: Entry) -> None:
        self._store.put(key, entry)

    def get(self, key: str):
        return self._store.get(key)

    def evicted(self) -> list:
        """Keys evicted so far, oldest eviction first."""
        return self._store.evicted()

    def load(self) -> int:
        return self._store.load()
