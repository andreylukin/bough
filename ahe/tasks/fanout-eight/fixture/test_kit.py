"""The checked-in suite. Covers the cases that already work."""

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


class TestKit(unittest.TestCase):
    def test_roman(self):
        self.assertEqual(to_roman(4), "IV")
        self.assertEqual(to_roman(1990), "MCMXC")

    def test_base_convert(self):
        self.assertEqual(convert("ff", 16, 10), "255")
        self.assertEqual(convert("255", 10, 16), "ff")

    def test_flatten_one_level(self):
        self.assertEqual(flatten([1, [2, 3], 4]), [1, 2, 3, 4])

    def test_median_odd(self):
        self.assertEqual(median([3, 1, 2]), 2)

    def test_transpose(self):
        self.assertEqual(transpose([[1, 2], [3, 4]]), [[1, 3], [2, 4]])

    def test_business_days_empty_range(self):
        self.assertEqual(business_days(date(2026, 3, 6), date(2026, 3, 2)), 0)

    def test_toposort_simple(self):
        self.assertEqual(toposort([("a", "b"), ("b", "c")], ["a", "b", "c"]), ["a", "b", "c"])

    def test_bank_happy_path(self):
        a = Account(500)
        a.deposit(100)
        self.assertEqual(a.cents, 600)
        a.withdraw(600)
        self.assertEqual(a.cents, 0)
        with self.assertRaises(InsufficientFunds):
            a.withdraw(1)


if __name__ == "__main__":
    unittest.main()
