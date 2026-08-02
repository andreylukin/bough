"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from plan import Filter, Join, Scan, pushdown

A = Scan("a", frozenset({"id", "x"}))
B = Scan("b", frozenset({"aid", "y"}))
C = Scan("c", frozenset({"bid", "z"}))

PX = ("cmp", "x", "=", 1)
PY = ("cmp", "y", "=", 2)
PZ = ("cmp", "z", "=", 3)


class TestLeftJoin(unittest.TestCase):
    """R3: the right side of a left join is off limits."""

    def test_right_side_predicate_stays_above(self):
        j = Join(A, B, ("id", "aid"), "left")
        out = pushdown(Filter(j, PY))
        self.assertEqual(out, Filter(j, PY))

    def test_left_side_predicate_still_pushes(self):
        j = Join(A, B, ("id", "aid"), "left")
        out = pushdown(Filter(j, PX))
        self.assertEqual(out, Join(Filter(A, PX), B, ("id", "aid"), "left"))

    def test_mixed_conjunction_splits_correctly(self):
        j = Join(A, B, ("id", "aid"), "left")
        out = pushdown(Filter(j, ("and", PX, PY)))
        self.assertEqual(
            out, Filter(Join(Filter(A, PX), B, ("id", "aid"), "left"), PY)
        )

    def test_inner_join_is_unaffected_by_the_rule(self):
        j = Join(A, B, ("id", "aid"), "inner")
        out = pushdown(Filter(j, PY))
        self.assertEqual(out, Join(A, Filter(B, PY), ("id", "aid"), "inner"))

    def test_nested_left_join_right_side(self):
        inner = Join(B, C, ("aid", "bid"), "left")
        j = Join(A, inner, ("id", "aid"), "inner")
        out = pushdown(Filter(j, PZ))
        # PZ needs only C, but C is the right side of a LEFT join.
        self.assertEqual(out, Join(A, Filter(inner, PZ), ("id", "aid"), "inner"))


class TestUnknownColumns(unittest.TestCase):
    """R4: an unplaceable column is an error, not a top-level filter."""

    def test_unknown_column_raises(self):
        with self.assertRaises(ValueError):
            pushdown(Filter(Join(A, B, ("id", "aid")), ("cmp", "nope", "=", 1)))

    def test_the_message_names_the_column(self):
        with self.assertRaises(ValueError) as cm:
            pushdown(Filter(A, ("cmp", "ghost", "=", 1)))
        self.assertIn("ghost", str(cm.exception))

    def test_unknown_inside_a_conjunction_raises(self):
        pred = ("and", PX, ("cmp", "ghost", "=", 1))
        with self.assertRaises(ValueError):
            pushdown(Filter(Join(A, B, ("id", "aid")), pred))


class TestDisjunctions(unittest.TestCase):
    """R1 + R2: an OR moves only as a unit."""

    def test_or_within_one_side_pushes(self):
        pred = ("or", PX, ("cmp", "id", "=", 9))
        out = pushdown(Filter(Join(A, B, ("id", "aid")), pred))
        self.assertEqual(out, Join(Filter(A, pred), B, ("id", "aid"), "inner"))

    def test_or_spanning_both_sides_stays_above(self):
        pred = ("or", PX, PY)
        j = Join(A, B, ("id", "aid"))
        self.assertEqual(pushdown(Filter(j, pred)), Filter(j, pred))


class TestOrder(unittest.TestCase):
    """R5: the first conjunct ends up innermost."""

    def test_two_predicates_on_one_scan(self):
        p1 = ("cmp", "x", "=", 1)
        p2 = ("cmp", "id", "=", 2)
        out = pushdown(Filter(A, ("and", p1, p2)))
        self.assertEqual(out, Filter(Filter(A, p1), p2))


if __name__ == "__main__":
    unittest.main()
