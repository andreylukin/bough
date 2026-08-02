"""The grading suite. Never present in the workspace; copied in by verify.sh.

Each test names the spec rule it enforces.
"""

import unittest

from lru import Cache, Entry
from lru.store import CapacityError


class TestCapacityIsCost(unittest.TestCase):
    """R1: capacity bounds the SUM of costs, not the number of entries."""

    def test_load_is_the_sum_of_costs(self):
        c = Cache(capacity=10)
        c.put("a", Entry("A", cost=3))
        c.put("b", Entry("B", cost=4))
        self.assertEqual(c.load(), 7)

    def test_two_entries_can_overflow_capacity(self):
        c = Cache(capacity=10)
        c.put("a", Entry("A", cost=6))
        c.put("b", Entry("B", cost=6))
        self.assertEqual(c.evicted(), ["a"])
        self.assertEqual(c.load(), 6)

    def test_many_cheap_entries_all_fit(self):
        c = Cache(capacity=10)
        for i in range(10):
            c.put(f"k{i}", Entry(i, cost=1))
        self.assertEqual(c.evicted(), [])
        self.assertEqual(c.load(), 10)


class TestRecency(unittest.TestCase):
    """R2: a hit is a use; a miss is not."""

    def test_get_refreshes_recency(self):
        c = Cache(capacity=2)
        c.put("a", Entry("A"))
        c.put("b", Entry("B"))
        c.get("a")
        c.put("c", Entry("C"))
        self.assertEqual(c.evicted(), ["b"])
        self.assertEqual(c.get("a"), "A")

    def test_a_miss_does_not_refresh_anything(self):
        c = Cache(capacity=2)
        c.put("a", Entry("A"))
        c.put("b", Entry("B"))
        c.get("zzz")
        c.put("c", Entry("C"))
        self.assertEqual(c.evicted(), ["a"])


class TestPinning(unittest.TestCase):
    """R3: pinned entries are never evicted."""

    def test_pinned_is_skipped_in_favour_of_a_newer_unpinned(self):
        c = Cache(capacity=2)
        c.put("a", Entry("A", pinned=True))
        c.put("b", Entry("B"))
        c.put("c", Entry("C"))
        self.assertEqual(c.evicted(), ["b"])
        self.assertEqual(c.get("a"), "A")

    def test_all_pinned_raises_and_changes_nothing(self):
        c = Cache(capacity=2)
        c.put("a", Entry("A", pinned=True))
        c.put("b", Entry("B", pinned=True))
        with self.assertRaises(CapacityError):
            c.put("c", Entry("C"))
        self.assertEqual(c.evicted(), [])
        self.assertEqual(c.load(), 2)
        self.assertIsNone(c.get("c"))
        self.assertEqual(c.get("a"), "A")
        self.assertEqual(c.get("b"), "B")


class TestOversized(unittest.TestCase):
    """R4: an entry that can never fit evicts nothing."""

    def test_oversized_put_raises(self):
        c = Cache(capacity=5)
        with self.assertRaises(CapacityError):
            c.put("big", Entry("BIG", cost=6))

    def test_oversized_put_leaves_the_cache_untouched(self):
        c = Cache(capacity=5)
        c.put("a", Entry("A", cost=2))
        c.put("b", Entry("B", cost=2))
        with self.assertRaises(CapacityError):
            c.put("big", Entry("BIG", cost=99))
        self.assertEqual(c.evicted(), [])
        self.assertEqual(c.load(), 4)
        self.assertEqual(c.get("a"), "A")
        self.assertEqual(c.get("b"), "B")
        self.assertIsNone(c.get("big"))


class TestReplacement(unittest.TestCase):
    """R5: replacement updates cost and pinning, and is not an eviction."""

    def test_replacement_updates_load(self):
        c = Cache(capacity=10)
        c.put("a", Entry("A", cost=2))
        c.put("a", Entry("A2", cost=5))
        self.assertEqual(c.load(), 5)
        self.assertEqual(c.get("a"), "A2")

    def test_replacement_is_not_an_eviction(self):
        c = Cache(capacity=4)
        c.put("a", Entry("A", cost=4))
        c.put("a", Entry("A2", cost=4))
        self.assertEqual(c.evicted(), [])
        self.assertEqual(c.get("a"), "A2")

    def test_replacement_can_drop_the_pin(self):
        c = Cache(capacity=2)
        c.put("a", Entry("A", pinned=True))
        c.put("a", Entry("A2", pinned=False))
        c.put("b", Entry("B"))
        c.put("c", Entry("C"))
        self.assertEqual(c.evicted(), ["a"])

    def test_growing_a_replacement_evicts_someone_else(self):
        c = Cache(capacity=6)
        c.put("a", Entry("A", cost=2))
        c.put("b", Entry("B", cost=2))
        c.put("a", Entry("A2", cost=5))
        self.assertEqual(c.evicted(), ["b"])
        self.assertEqual(c.load(), 5)


if __name__ == "__main__":
    unittest.main()
