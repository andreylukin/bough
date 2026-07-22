import unittest

import base_convert
import bank
import calendar_days
import flatten
import graph
import matrix
import roman
import stats


class TestRoman(unittest.TestCase):
    def test_to(self):
        self.assertEqual(roman.to_roman(4), "IV")
        self.assertEqual(roman.to_roman(9), "IX")
        self.assertEqual(roman.to_roman(1994), "MCMXCIV")
        self.assertEqual(roman.to_roman(3888), "MMMDCCCLXXXVIII")

    def test_from(self):
        self.assertEqual(roman.from_roman("IV"), 4)
        self.assertEqual(roman.from_roman("MCMXCIV"), 1994)

    def test_roundtrip(self):
        for n in (1, 14, 40, 90, 400, 949, 2023):
            self.assertEqual(roman.from_roman(roman.to_roman(n)), n)


class TestMatrix(unittest.TestCase):
    def test_transpose(self):
        self.assertEqual(matrix.transpose([[1, 2, 3], [4, 5, 6]]),
                         [[1, 4], [2, 5], [3, 6]])

    def test_matmul(self):
        a = [[1, 2], [3, 4]]
        b = [[5, 6], [7, 8]]
        self.assertEqual(matrix.matmul(a, b), [[19, 22], [43, 50]])

    def test_matmul_identity(self):
        a = [[1, 2, 3], [4, 5, 6]]
        self.assertEqual(matrix.matmul(a, matrix.identity(3)), a)


class TestCalendar(unittest.TestCase):
    def test_leap(self):
        self.assertTrue(calendar_days.is_leap(2000))
        self.assertFalse(calendar_days.is_leap(1900))
        self.assertTrue(calendar_days.is_leap(2024))

    def test_days_between(self):
        # 2020 is a leap year: Feb 28 -> Mar 1 spans Feb 29.
        self.assertEqual(calendar_days.days_between((2020, 2, 28), (2020, 3, 1)), 2)
        self.assertEqual(calendar_days.days_between((2021, 1, 1), (2022, 1, 1)), 365)
        self.assertEqual(calendar_days.days_between((2020, 1, 1), (2021, 1, 1)), 366)


class TestBank(unittest.TestCase):
    def test_overdraft_boundary(self):
        a = bank.Account(balance=100, overdraft=50)
        a.withdraw(150)  # down to exactly -50, the limit — allowed
        self.assertEqual(a.balance, -50)

    def test_overdraft_exceeded(self):
        a = bank.Account(balance=100, overdraft=50)
        with self.assertRaises(ValueError):
            a.withdraw(151)

    def test_available(self):
        self.assertEqual(bank.Account(balance=20, overdraft=30).available(), 50)


class TestGraph(unittest.TestCase):
    def setUp(self):
        self.adj = {"a": ["b", "c"], "b": ["e"], "c": ["d"], "d": ["e"], "e": []}

    def test_shortest(self):
        # a->b->e (len 3) is shorter than a->c->d->e (len 4); a DFS would take
        # the longer route.
        self.assertEqual(graph.shortest_path(self.adj, "a", "e"),
                         ["a", "b", "e"])

    def test_same(self):
        self.assertEqual(graph.shortest_path(self.adj, "a", "a"), ["a"])

    def test_unreachable(self):
        self.assertIsNone(graph.shortest_path(self.adj, "e", "a"))


class TestFlatten(unittest.TestCase):
    def test_nested(self):
        self.assertEqual(
            flatten.flatten({"a": {"b": 1, "c": {"d": 2}}, "e": 3}),
            {"a.b": 1, "a.c.d": 2, "e": 3},
        )

    def test_list_is_leaf(self):
        self.assertEqual(flatten.flatten({"a": [1, 2], "b": 3}),
                         {"a": [1, 2], "b": 3})


class TestStats(unittest.TestCase):
    def test_median_odd(self):
        self.assertEqual(stats.median([3, 1, 2]), 2)

    def test_median_even(self):
        self.assertEqual(stats.median([4, 1, 3, 2]), 2.5)

    def test_variance(self):
        # sample variance of 1,2,3,4,5 is 2.5 (divide by n-1)
        self.assertEqual(stats.variance([1, 2, 3, 4, 5]), 2.5)


class TestBaseConvert(unittest.TestCase):
    def test_to(self):
        self.assertEqual(base_convert.to_base(255, 16), "ff")
        self.assertEqual(base_convert.to_base(10, 2), "1010")
        self.assertEqual(base_convert.to_base(0, 5), "0")

    def test_from(self):
        self.assertEqual(base_convert.from_base("ff", 16), 255)
        self.assertEqual(base_convert.from_base("1010", 2), 10)

    def test_roundtrip(self):
        for n in (0, 1, 35, 1000, 65535):
            for base in (2, 8, 16, 36):
                self.assertEqual(base_convert.from_base(base_convert.to_base(n, base), base), n)


if __name__ == "__main__":
    unittest.main()
