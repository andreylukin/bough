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
        return sum(e.cost for e in self._entries.values())

    def evicted(self):
        return list(self._evicted)

    def _evictable(self):
        """Unpinned keys, least recently used first."""
        keys = [k for k, e in self._entries.items() if not e.pinned]
        return sorted(keys, key=lambda k: self._used[k])

    def put(self, key, entry):
        # R4: an entry that can never fit changes nothing.
        if entry.cost > self.capacity:
            raise CapacityError(key)

        # R5: a replacement frees the old cost before the new one is weighed.
        prior = self._entries.get(key)
        load = self.load() - (prior.cost if prior is not None else 0)

        # R3: plan the eviction before applying any of it, so a plan that cannot
        # succeed leaves the cache untouched.
        plan = []
        for victim in self._evictable():
            if load + entry.cost <= self.capacity:
                break
            if victim == key:
                continue
            load -= self._entries[victim].cost
            plan.append(victim)
        if load + entry.cost > self.capacity:
            raise CapacityError(key)

        for victim in plan:
            del self._entries[victim]
            del self._used[victim]
            self._evicted.append(victim)

        self._entries[key] = entry
        self._stamp(key)

    def get(self, key):
        entry = self._entries.get(key)
        if entry is None:
            return None
        self._stamp(key)  # R2: a hit is a use.
        return entry.value
