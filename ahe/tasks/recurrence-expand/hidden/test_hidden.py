"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest
from datetime import date

from rrule import Rule, expand


class TestWeeklyByday(unittest.TestCase):
    """R3: first week truncated at start; ascending regardless of byday order."""

    def test_the_first_week_does_not_reach_back_before_start(self):
        # 2026-03-04 is a Wednesday; Monday of that week is 2026-03-02.
        r = Rule("weekly", date(2026, 3, 4), byday=(1, 3, 5), count=4)
        self.assertEqual(
            expand(r),
            [date(2026, 3, 4), date(2026, 3, 6), date(2026, 3, 9), date(2026, 3, 11)],
        )

    def test_byday_given_out_of_order_still_comes_back_ascending(self):
        r = Rule("weekly", date(2026, 3, 2), byday=(5, 1), count=4)
        self.assertEqual(
            expand(r),
            [date(2026, 3, 2), date(2026, 3, 6), date(2026, 3, 9), date(2026, 3, 13)],
        )

    def test_interval_two_skips_a_whole_week(self):
        r = Rule("weekly", date(2026, 3, 2), byday=(1, 3), interval=2, count=4)
        self.assertEqual(
            expand(r),
            [date(2026, 3, 2), date(2026, 3, 4), date(2026, 3, 16), date(2026, 3, 18)],
        )


class TestMonthlySkips(unittest.TestCase):
    """R4: a month without the day is skipped, never clamped."""

    def test_the_31st_skips_short_months(self):
        r = Rule("monthly", date(2026, 1, 31), count=4)
        self.assertEqual(
            expand(r),
            [date(2026, 1, 31), date(2026, 3, 31), date(2026, 5, 31), date(2026, 7, 31)],
        )

    def test_the_30th_skips_only_february(self):
        r = Rule("monthly", date(2026, 1, 30), count=3)
        self.assertEqual(
            expand(r), [date(2026, 1, 30), date(2026, 3, 30), date(2026, 4, 30)]
        )

    def test_a_skip_does_not_shift_the_phase(self):
        # Every 2 months from Dec 31 lands on Feb/Apr/Jun 31 — none of which exist —
        # so the next emitted date is Aug 31. Re-phasing on emitted occurrences
        # instead of calendar months would give a different, wrong answer.
        r = Rule("monthly", date(2025, 12, 31), interval=2, count=3)
        self.assertEqual(
            expand(r),
            [date(2025, 12, 31), date(2026, 8, 31), date(2026, 10, 31)],
        )

    def test_february_29_recurs_only_in_leap_years(self):
        r = Rule("monthly", date(2024, 2, 29), interval=12, count=2)
        self.assertEqual(expand(r), [date(2024, 2, 29), date(2028, 2, 29)])


class TestStopping(unittest.TestCase):
    """R5: until is inclusive; exclusions do not consume count."""

    def test_until_is_inclusive(self):
        r = Rule("daily", date(2026, 3, 1), until=date(2026, 3, 3))
        self.assertEqual(
            expand(r), [date(2026, 3, 1), date(2026, 3, 2), date(2026, 3, 3)]
        )

    def test_an_excluded_date_does_not_consume_count(self):
        r = Rule(
            "daily",
            date(2026, 3, 1),
            count=3,
            exclude=frozenset({date(2026, 3, 2)}),
        )
        self.assertEqual(
            expand(r), [date(2026, 3, 1), date(2026, 3, 3), date(2026, 3, 4)]
        )

    def test_until_can_stop_before_count_is_reached(self):
        r = Rule(
            "daily",
            date(2026, 3, 1),
            count=10,
            until=date(2026, 3, 2),
            exclude=frozenset({date(2026, 3, 1)}),
        )
        self.assertEqual(expand(r), [date(2026, 3, 2)])


if __name__ == "__main__":
    unittest.main()
