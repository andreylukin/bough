import unittest

from logs import Event, Logs


def ev(id, start, end, *tags):
    return Event(id, start, end, list(tags))


CORPUS = [
    ev(1, 0, 10, "a", "b"),
    ev(2, 5, 15, "b"),
    ev(3, 20, 30, "c"),
    ev(4, 25, 40, "c", "a"),
]


class TestOverlapping(unittest.TestCase):
    def setUp(self):
        self.logs = Logs(CORPUS)

    def test_window_in_the_middle(self):
        self.assertEqual([e.id for e in self.logs.overlapping(6, 12)], [1, 2])

    def test_window_covering_everything(self):
        self.assertEqual([e.id for e in self.logs.overlapping(0, 100)], [1, 2, 3, 4])

    def test_results_are_ordered_by_start_then_id(self):
        self.assertEqual([e.id for e in self.logs.overlapping(20, 35)], [3, 4])

    def test_empty_window(self):
        self.assertEqual(self.logs.overlapping(50, 60), [])


class TestTopTags(unittest.TestCase):
    def setUp(self):
        self.logs = Logs(CORPUS)

    def test_top_tag_over_everything(self):
        # a, b and c all appear twice; "a" wins either way this is broken down, so
        # the assertion is deliberately blind to the tie-break rule.
        self.assertEqual(self.logs.top_tags(0, 100, 1), [("a", 2)])

    def test_k_limits_the_result(self):
        self.assertEqual(len(self.logs.top_tags(0, 100, 2)), 2)

    def test_counts_only_the_window(self):
        self.assertEqual(self.logs.top_tags(20, 35, 5)[0], ("c", 2))


if __name__ == "__main__":
    unittest.main()
