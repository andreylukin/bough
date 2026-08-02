"""The checked-in suite. Covers the cases that already work."""

import unittest

from money import allocate, apply_rate


class TestAllocate(unittest.TestCase):
    def test_exact_split(self):
        self.assertEqual(allocate(100, [1, 1]), [50, 50])

    def test_exact_three_way(self):
        self.assertEqual(allocate(90, [1, 1, 1]), [30, 30, 30])

    def test_weighted_exact(self):
        self.assertEqual(allocate(100, [3, 1]), [75, 25])

    def test_empty_weights(self):
        self.assertEqual(allocate(100, []), [])

    def test_zero_weights_rejected(self):
        with self.assertRaises(ValueError):
            allocate(100, [0, 0])

    def test_negative_weight_rejected(self):
        with self.assertRaises(ValueError):
            allocate(100, [1, -1])


class TestRate(unittest.TestCase):
    def test_simple_rate(self):
        self.assertEqual(apply_rate(1000, 1, 4), 250)

    def test_zero_denominator(self):
        with self.assertRaises(ValueError):
            apply_rate(100, 1, 0)


if __name__ == "__main__":
    unittest.main()
