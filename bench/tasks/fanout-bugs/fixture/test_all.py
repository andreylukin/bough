import unittest

from mod_a import average
from mod_b import last_n
from mod_c import passing
from mod_d import collect


class TestA(unittest.TestCase):
    def test_mean(self):
        self.assertEqual(average([2, 4]), 3)

    def test_empty(self):
        self.assertEqual(average([]), 0)


class TestB(unittest.TestCase):
    def test_last_two(self):
        self.assertEqual(last_n([1, 2, 3, 4, 5], 2), [4, 5])

    def test_last_three(self):
        self.assertEqual(last_n([1, 2, 3, 4, 5], 3), [3, 4, 5])


class TestC(unittest.TestCase):
    def test_boundary(self):
        self.assertTrue(passing(60))

    def test_below(self):
        self.assertFalse(passing(59))


class TestD(unittest.TestCase):
    def test_no_shared_state(self):
        self.assertEqual(collect(1), [1])
        self.assertEqual(collect(2), [2])


if __name__ == "__main__":
    unittest.main()
