"""The checked-in suite. Covers the behaviour that already works."""

import unittest
from datetime import date

from rrule import Rule, expand


class TestDaily(unittest.TestCase):
    def test_every_day(self):
        r = Rule("daily", date(2026, 3, 1), count=4)
        self.assertEqual(
            expand(r),
            [date(2026, 3, 1), date(2026, 3, 2), date(2026, 3, 3), date(2026, 3, 4)],
        )

    def test_every_third_day(self):
        r = Rule("daily", date(2026, 3, 1), interval=3, count=3)
        self.assertEqual(expand(r), [date(2026, 3, 1), date(2026, 3, 4), date(2026, 3, 7)])


class TestWeekly(unittest.TestCase):
    def test_same_weekday_every_week(self):
        # 2026-03-02 is a Monday.
        r = Rule("weekly", date(2026, 3, 2), count=3)
        self.assertEqual(expand(r), [date(2026, 3, 2), date(2026, 3, 9), date(2026, 3, 16)])

    def test_byday_from_a_monday_start(self):
        r = Rule("weekly", date(2026, 3, 2), byday=(1, 3), count=4)
        self.assertEqual(
            expand(r),
            [date(2026, 3, 2), date(2026, 3, 4), date(2026, 3, 9), date(2026, 3, 11)],
        )


class TestMonthly(unittest.TestCase):
    def test_same_day_each_month(self):
        r = Rule("monthly", date(2026, 1, 15), count=3)
        self.assertEqual(
            expand(r), [date(2026, 1, 15), date(2026, 2, 15), date(2026, 3, 15)]
        )

    def test_interval_two_crosses_the_year(self):
        r = Rule("monthly", date(2026, 11, 5), interval=2, count=3)
        self.assertEqual(
            expand(r), [date(2026, 11, 5), date(2027, 1, 5), date(2027, 3, 5)]
        )


class TestStopping(unittest.TestCase):
    def test_count_stops(self):
        r = Rule("daily", date(2026, 3, 1), count=2)
        self.assertEqual(len(expand(r)), 2)

    def test_a_rule_with_no_stop_is_rejected(self):
        with self.assertRaises(ValueError):
            expand(Rule("daily", date(2026, 3, 1)))


if __name__ == "__main__":
    unittest.main()
