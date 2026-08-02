"""The cache internals."""


class CapacityError(Exception):
    pass


class Store:
    def __init__(self, capacity: int, clock=None):
        self.capacity = capacity
        self._entries = {}
        # key -> a monotonically increasing stamp; higher is more recent.
        self._used = {}
        self._seq = 0
        self._evicted = []

    def _stamp(self, key):
        self._seq += 1
        self._used[key] = self._seq

    def load(self):
        return len(self._entries)

    def evicted(self):
        return list(self._evicted)

    def _victim(self):
        return min(self._entries, key=lambda k: self._used[k])

    def _make_room(self):
        while len(self._entries) > self.capacity:
            victim = self._victim()
            del self._entries[victim]
            del self._used[victim]
            self._evicted.append(victim)

    def put(self, key, entry):
        if entry.cost > self.capacity:
            self._entries[key] = entry
            self._stamp(key)
            self._make_room()
            raise CapacityError(key)
        self._entries[key] = entry
        self._stamp(key)
        self._make_room()

    def get(self, key):
        entry = self._entries.get(key)
        if entry is None:
            return None
        return entry.value
