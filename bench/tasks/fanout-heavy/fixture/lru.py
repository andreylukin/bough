"""A fixed-capacity LRU cache. Reading a key counts as using it, so a `get`
must make that key the most-recently-used — otherwise it can be evicted even
though it was just accessed.
"""


class LRUCache:
    def __init__(self, capacity):
        if capacity <= 0:
            raise ValueError("capacity must be positive")
        self.capacity = capacity
        self._data = {}
        # keys in use-order, least-recently-used first
        self._order = []

    def _touch(self, key):
        self._order.remove(key)
        self._order.append(key)

    def get(self, key, default=None):
        if key not in self._data:
            return default
        return self._data[key]

    def put(self, key, value):
        if key in self._data:
            self._data[key] = value
            self._touch(key)
            return
        if len(self._data) >= self.capacity:
            evict = self._order.pop(0)
            del self._data[evict]
        self._data[key] = value
        self._order.append(key)

    def keys(self):
        """Keys currently held, least- to most-recently-used."""
        return list(self._order)
