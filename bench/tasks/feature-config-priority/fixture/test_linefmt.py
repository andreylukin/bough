import unittest

from textutil import wrap


class TestWrap(unittest.TestCase):
    def test_wraps_greedily(self):
        self.assertEqual(wrap("aa bb cc", 5), ["aa bb", "cc"])

    def test_short_line_unchanged(self):
        self.assertEqual(wrap("hello", 60), ["hello"])

    def test_empty_line(self):
        self.assertEqual(wrap("", 10), [""])


if __name__ == "__main__":
    unittest.main()
