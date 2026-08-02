"""The checked-in suite. Covers the cases that already work."""

import unittest

from plan import Filter, Join, Scan, pushdown

A = Scan("a", frozenset({"id", "x"}))
B = Scan("b", frozenset({"aid", "y"}))


class TestPushdown(unittest.TestCase):
    def test_single_predicate_reaches_the_left_scan(self):
        p = Filter(Join(A, B, ("id", "aid")), ("cmp", "x", "=", 1))
        out = pushdown(p)
        self.assertIsInstance(out, Join)
        self.assertEqual(out.left, Filter(A, ("cmp", "x", "=", 1)))
        self.assertEqual(out.right, B)

    def test_conjunction_splits_to_both_sides(self):
        pred = ("and", ("cmp", "x", "=", 1), ("cmp", "y", "=", 2))
        out = pushdown(Filter(Join(A, B, ("id", "aid")), pred))
        self.assertEqual(out.left, Filter(A, ("cmp", "x", "=", 1)))
        self.assertEqual(out.right, Filter(B, ("cmp", "y", "=", 2)))

    def test_scan_alone_is_unchanged(self):
        self.assertEqual(pushdown(A), A)

    def test_filter_on_a_scan_stays(self):
        f = Filter(A, ("cmp", "x", "=", 1))
        self.assertEqual(pushdown(f), f)


if __name__ == "__main__":
    unittest.main()
