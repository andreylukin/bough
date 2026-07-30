"""The grading suite. Never present in the workspace; copied in by verify.sh.

Correctness first, then the budget. The budget test is last on purpose: a solution
that is fast and wrong should read as wrong, not as slow.
"""

import random
import time
import unittest

from logs import Event, Logs


def ev(id, start, end, *tags):
    return Event(id, start, end, list(tags))


class TestHalfOpenIntervals(unittest.TestCase):
    """Spec R1: `start < t1 and t0 < end`. Touching is not overlapping."""

    def setUp(self):
        self.logs = Logs([ev(1, 0, 10, "a"), ev(2, 10, 20, "b"), ev(3, 20, 30, "c")])

    def test_event_ending_exactly_at_t0_does_not_overlap(self):
        self.assertEqual([e.id for e in self.logs.overlapping(10, 15)], [2])

    def test_event_starting_exactly_at_t1_does_not_overlap(self):
        self.assertEqual([e.id for e in self.logs.overlapping(5, 10)], [1])

    def test_empty_window_matches_nothing(self):
        self.assertEqual(self.logs.overlapping(10, 10), [])

    def test_window_strictly_inside_one_event(self):
        self.assertEqual([e.id for e in self.logs.overlapping(11, 12)], [2])


class TestOrdering(unittest.TestCase):
    """Spec R1: by start, then by id — regardless of input order."""

    def test_same_start_orders_by_id(self):
        logs = Logs([ev(9, 5, 9, "a"), ev(2, 5, 9, "a"), ev(7, 1, 9, "a")])
        self.assertEqual([e.id for e in logs.overlapping(0, 100)], [7, 2, 9])


class TestFirstAppearanceTieBreak(unittest.TestCase):
    """Spec R2: ties go to the tag seen first in rule 1's ordering, not to 'a'."""

    def test_tie_goes_to_first_appearance_not_alphabetical(self):
        logs = Logs([ev(1, 0, 5, "zebra"), ev(2, 1, 6, "apple")])
        self.assertEqual(logs.top_tags(0, 100, 2), [("zebra", 1), ("apple", 1)])

    def test_three_way_tie_keeps_appearance_order(self):
        logs = Logs([ev(1, 0, 5, "m"), ev(2, 1, 6, "c"), ev(3, 2, 7, "x")])
        self.assertEqual([t for t, _ in logs.top_tags(0, 100, 3)], ["m", "c", "x"])

    def test_frequency_still_dominates_the_tie_break(self):
        logs = Logs([ev(1, 0, 5, "zebra"), ev(2, 1, 6, "apple"), ev(3, 2, 7, "apple")])
        self.assertEqual(logs.top_tags(0, 100, 1), [("apple", 2)])

    def test_tie_break_respects_the_window(self):
        # "zebra" appears first in the corpus but not in THIS window, so "apple"
        # is the first appearance here.
        logs = Logs([ev(1, 0, 5, "zebra"), ev(2, 50, 60, "apple"), ev(3, 51, 61, "zebra")])
        self.assertEqual([t for t, _ in logs.top_tags(50, 62, 2)], ["apple", "zebra"])


def corpus(n, seed=7):
    rng = random.Random(seed)
    events = []
    for i in range(n):
        start = rng.randrange(0, 2_000_000)
        events.append(
            Event(i, start, start + rng.randrange(1, 500), [f"t{rng.randrange(50)}"])
        )
    return events


class TestAgreesWithTheObviousImplementation(unittest.TestCase):
    """Randomized, against a slow but plainly-correct reference."""

    def test_matches_a_linear_scan_on_random_windows(self):
        events = corpus(2_000, seed=11)
        logs = Logs(events)
        rng = random.Random(3)
        # Half the windows are aligned to real event boundaries. Random windows
        # almost never land exactly on a start or an end, so an off-by-one in the
        # overlap test survives them — whatever shape the implementation takes.
        edges = [e.start for e in events] + [e.end for e in events]
        for i in range(400):
            if i % 2 == 0:
                t0 = rng.choice(edges)
                t1 = t0 + rng.choice([0, 1, 1, 5, 500])
            else:
                t0 = rng.randrange(0, 2_000_000)
                t1 = t0 + rng.randrange(1, 5_000)
            want = sorted(
                (e for e in events if e.start < t1 and t0 < e.end),
                key=lambda e: (e.start, e.id),
            )
            got = logs.overlapping(t0, t1)
            self.assertEqual([e.id for e in got], [e.id for e in want], f"window {t0},{t1}")


class TestBudget(unittest.TestCase):
    """Spec R3: build once, then answer queries without rescanning the corpus."""

    def test_many_queries_over_a_large_corpus(self):
        events = corpus(200_000)
        built = time.perf_counter()
        logs = Logs(events)
        build_s = time.perf_counter() - built

        rng = random.Random(5)
        windows = [
            (lambda t0: (t0, t0 + rng.randrange(1, 20_000)))(rng.randrange(0, 2_000_000))
            for _ in range(1_000)
        ]
        started = time.perf_counter()
        total = 0
        for t0, t1 in windows:
            total += len(logs.overlapping(t0, t1))
        query_s = time.perf_counter() - started

        self.assertGreater(total, 0, "the queries matched nothing — check correctness first")
        # The reference answers these in well under a second. A per-query scan of
        # 200k events takes minutes. The gate sits far from both.
        # CALIBRATION. The gate has to separate ALGORITHMS, not machines. The
        # first version sat at 3.0s and a prefix-bounded linear scan came in at
        # 2.58s — so two trials passed and two failed on identical code, and the
        # bench was reporting timing jitter as capability. Measured here: the
        # reference indexes both edges and answers 1,000 queries in ~0.2s; the
        # prefix-only scan every trial actually wrote takes 5-11s. The gate sits an
        # order of magnitude from each.
        self.assertLess(
            query_s,
            2.0,
            f"1,000 queries took {query_s:.1f}s (build {build_s:.1f}s) — "
            "the budget is 2s; queries are still scanning the corpus",
        )


if __name__ == "__main__":
    unittest.main()
