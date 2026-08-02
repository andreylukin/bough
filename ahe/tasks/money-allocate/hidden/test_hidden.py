"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from money import allocate, apply_rate


class TestConservation(unittest.TestCase):
    """R1 + R2: the total is conserved, by largest remainder."""

    def test_thirds_of_a_dollar(self):
        self.assertEqual(allocate(100, [1, 1, 1]), [34, 33, 33])

    def test_seven_ways(self):
        out = allocate(100, [1] * 7)
        self.assertEqual(sum(out), 100)
        self.assertEqual(out, [15, 15, 14, 14, 14, 14, 14])

    def test_lopsided_weights(self):
        out = allocate(1000, [1, 1, 1, 97])
        self.assertEqual(sum(out), 1000)
        self.assertEqual(out, [10, 10, 10, 970])

    def test_conserved_across_many_shapes(self):
        for total in (1, 7, 99, 100, 12345):
            for weights in ([1, 2], [1, 1, 1], [5, 3, 2], [1] * 9, [7, 11, 13]):
                self.assertEqual(sum(allocate(total, weights)), total, (total, weights))

    def test_a_tie_goes_to_the_earlier_weight(self):
        # Two equal remainders, one leftover cent: index 0 takes it.
        self.assertEqual(allocate(1, [1, 1]), [1, 0])


class TestZeroWeights(unittest.TestCase):
    """R3: a zero weight is never paid."""

    def test_zero_weight_gets_nothing(self):
        out = allocate(100, [0, 1, 1, 1])
        self.assertEqual(out[0], 0)
        self.assertEqual(sum(out), 100)

    def test_zero_weight_takes_no_leftover(self):
        self.assertEqual(allocate(10, [0, 3]), [0, 10])


class TestNegativeTotals(unittest.TestCase):
    """R4: the negative case mirrors the positive one exactly."""

    def test_mirrors(self):
        for weights in ([1, 1, 1], [5, 3, 2], [1] * 7, [1, 2]):
            for total in (1, 100, 99, 12345):
                pos = allocate(total, weights)
                neg = allocate(-total, weights)
                self.assertEqual(neg, [-x for x in pos], (total, weights))

    def test_negative_total_is_conserved(self):
        self.assertEqual(sum(allocate(-100, [1, 1, 1])), -100)


class TestRate(unittest.TestCase):
    """R5: half away from zero, in integers."""

    def test_half_rounds_away_from_zero(self):
        self.assertEqual(apply_rate(5, 1, 2), 3)
        self.assertEqual(apply_rate(15, 1, 2), 8)
        self.assertEqual(apply_rate(-5, 1, 2), -3)

    def test_below_half_rounds_down(self):
        self.assertEqual(apply_rate(4, 1, 2), 2)
        self.assertEqual(apply_rate(-4, 1, 2), -2)

    def test_exact_beyond_float_precision(self):
        big = 10**18 + 1
        self.assertEqual(apply_rate(big, 1, 1), big)
        self.assertEqual(apply_rate(2 * big, 1, 2), big)


if __name__ == "__main__":
    unittest.main()
