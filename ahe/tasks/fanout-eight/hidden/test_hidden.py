"""The grading suite. Never present in the workspace; copied in by verify.sh.

One class per module: every one of the eight has to be right.
"""

import unittest
from datetime import date

from kit.bank import Account, InsufficientFunds
from kit.base_convert import convert
from kit.calendar_days import business_days
from kit.flatten import flatten
from kit.graph import toposort
from kit.matrix import transpose
from kit.roman import to_roman
from kit.stats import median


class TestRoman(unittest.TestCase):
    def test_repeated_symbols(self):
        self.assertEqual(to_roman(3), "III")
        self.assertEqual(to_roman(38), "XXXVIII")
        self.assertEqual(to_roman(2024), "MMXXIV")
        self.assertEqual(to_roman(3999), "MMMCMXCIX")

    def test_range_is_enforced(self):
        for bad in (0, 4000, -1):
            with self.assertRaises(ValueError):
                to_roman(bad)


class TestBaseConvert(unittest.TestCase):
    def test_zero_round_trips(self):
        self.assertEqual(convert("0", 10, 2), "0")
        self.assertEqual(convert("0", 16, 36), "0")

    def test_wide_bases(self):
        self.assertEqual(convert("zz", 36, 10), "1295")
        self.assertEqual(convert("1295", 10, 36), "zz")

    def test_bad_base_rejected(self):
        with self.assertRaises(ValueError):
            convert("1", 1, 10)


class TestFlatten(unittest.TestCase):
    def test_deep_nesting(self):
        self.assertEqual(flatten([1, [2, [3, [4, [5]]]]]), [1, 2, 3, 4, 5])

    def test_tuples_too(self):
        self.assertEqual(flatten([(1, 2), [3, (4,)]]), [1, 2, 3, 4])

    def test_strings_are_leaves(self):
        self.assertEqual(flatten(["ab", ["cd"]]), ["ab", "cd"])


class TestStats(unittest.TestCase):
    def test_even_length_averages_the_middle_two(self):
        self.assertEqual(median([1, 2, 3, 4]), 2.5)
        self.assertEqual(median([4, 1, 3, 2]), 2.5)

    def test_odd_length_unchanged(self):
        self.assertEqual(median([5, 1, 3]), 3)

    def test_empty_raises(self):
        with self.assertRaises(ValueError):
            median([])


class TestMatrix(unittest.TestCase):
    def test_ragged_raises(self):
        with self.assertRaises(ValueError):
            transpose([[1, 2], [3]])

    def test_non_square(self):
        self.assertEqual(transpose([[1, 2, 3], [4, 5, 6]]), [[1, 4], [2, 5], [3, 6]])

    def test_empty(self):
        self.assertEqual(transpose([]), [])


class TestCalendarDays(unittest.TestCase):
    def test_both_ends_are_inclusive(self):
        # Mon 2026-03-02 .. Fri 2026-03-06
        self.assertEqual(business_days(date(2026, 3, 2), date(2026, 3, 6)), 5)

    def test_weekend_is_skipped(self):
        # Fri .. following Mon
        self.assertEqual(business_days(date(2026, 3, 6), date(2026, 3, 9)), 2)

    def test_single_weekday(self):
        self.assertEqual(business_days(date(2026, 3, 3), date(2026, 3, 3)), 1)

    def test_single_weekend_day(self):
        self.assertEqual(business_days(date(2026, 3, 7), date(2026, 3, 7)), 0)


class TestGraph(unittest.TestCase):
    def test_cycle_raises(self):
        with self.assertRaises(ValueError):
            toposort([("a", "b"), ("b", "a")], ["a", "b"])

    def test_ties_are_broken_deterministically(self):
        order = toposort([("c", "a"), ("c", "b")], ["a", "b", "c"])
        self.assertEqual(order, ["c", "a", "b"])

    def test_wide_graph_stays_sorted(self):
        edges = [("root", n) for n in ["d", "b", "c", "a"]]
        order = toposort(edges, ["root", "a", "b", "c", "d"])
        self.assertEqual(order, ["root", "a", "b", "c", "d"])


class TestBank(unittest.TestCase):
    def test_a_refused_withdrawal_leaves_the_balance_alone(self):
        a = Account(100)
        with self.assertRaises(InsufficientFunds):
            a.withdraw(500)
        self.assertEqual(a.cents, 100)

    def test_negative_amounts_rejected(self):
        a = Account(100)
        with self.assertRaises(ValueError):
            a.withdraw(-1)
        with self.assertRaises(ValueError):
            a.deposit(-1)
        self.assertEqual(a.cents, 100)


if __name__ == "__main__":
    unittest.main()
