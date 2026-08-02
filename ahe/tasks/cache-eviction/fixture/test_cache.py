"""The checked-in suite. Covers the behaviour that already works."""

import unittest

from lru import Cache, Entry


class TestBasics(unittest.TestCase):
    def test_put_then_get(self):
        c = Cache(capacity=4)
        c.put("a", Entry("A"))
        self.assertEqual(c.get("a"), "A")

    def test_miss_is_none(self):
        c = Cache(capacity=4)
        self.assertIsNone(c.get("nope"))

    def test_replacing_a_key_returns_the_new_value(self):
        c = Cache(capacity=4)
        c.put("a", Entry("A"))
        c.put("a", Entry("A2"))
        self.assertEqual(c.get("a"), "A2")


class TestEviction(unittest.TestCase):
    def test_evicts_the_least_recently_put(self):
        c = Cache(capacity=2)
        c.put("a", Entry("A"))
        c.put("b", Entry("B"))
        c.put("c", Entry("C"))
        self.assertEqual(c.evicted(), ["a"])
        self.assertIsNone(c.get("a"))
        self.assertEqual(c.get("b"), "B")
        self.assertEqual(c.get("c"), "C")

    def test_eviction_order_is_oldest_first(self):
        c = Cache(capacity=2)
        for k in ["a", "b", "c", "d"]:
            c.put(k, Entry(k.upper()))
        self.assertEqual(c.evicted(), ["a", "b"])

    def test_nothing_is_evicted_while_under_capacity(self):
        c = Cache(capacity=3)
        c.put("a", Entry("A"))
        c.put("b", Entry("B"))
        self.assertEqual(c.evicted(), [])


if __name__ == "__main__":
    unittest.main()
