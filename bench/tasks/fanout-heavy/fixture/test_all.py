import unittest

from intervals import merge, total_covered
from lru import LRUCache
from percentile import percentile, median
from tokenizer import split
from ledger import net_cents, is_balanced
from rpn import evaluate


class TestIntervals(unittest.TestCase):
    def test_touching_merge(self):
        # ranges that touch at a point are one contiguous range
        self.assertEqual(merge([[1, 2], [2, 3]]), [[1, 3]])

    def test_overlap_and_gap(self):
        self.assertEqual(merge([[1, 4], [2, 3], [6, 8]]), [[1, 4], [6, 8]])

    def test_total_covered(self):
        self.assertEqual(total_covered([[1, 2], [2, 3], [10, 12]]), 4)


class TestLRU(unittest.TestCase):
    def test_get_marks_recently_used(self):
        c = LRUCache(2)
        c.put("a", 1)
        c.put("b", 2)
        c.get("a")          # 'a' is now most-recently-used
        c.put("c", 3)       # so 'b' should be evicted, not 'a'
        self.assertEqual(c.get("a"), 1)
        self.assertIsNone(c.get("b"))


class TestPercentile(unittest.TestCase):
    def test_median_even(self):
        self.assertEqual(median([1, 2, 3, 4]), 2.5)

    def test_interp(self):
        self.assertAlmostEqual(percentile([0, 10], 25), 2.5)


class TestTokenizer(unittest.TestCase):
    def test_quoted_and_trailing(self):
        self.assertEqual(split('a "b c" d'), ["a", "b c", "d"])

    def test_no_trailing_space(self):
        self.assertEqual(split("one two"), ["one", "two"])


class TestLedger(unittest.TestCase):
    def test_net(self):
        entries = [{"kind": "credit", "cents": 500}, {"kind": "debit", "cents": 200}]
        self.assertEqual(net_cents(entries), 300)

    def test_balanced(self):
        self.assertTrue(is_balanced(
            [{"kind": "credit", "cents": 100}, {"kind": "debit", "cents": 100}]
        ))


class TestRPN(unittest.TestCase):
    def test_subtraction(self):
        self.assertEqual(evaluate(["6", "2", "-"]), 4.0)

    def test_division(self):
        self.assertEqual(evaluate(["6", "2", "/"]), 3.0)

    def test_commutative_chain(self):
        self.assertEqual(evaluate(["3", "4", "+", "2", "*"]), 14.0)


if __name__ == "__main__":
    unittest.main()
