"""The checked-in suite. Covers the cases that already work."""

import unittest

from tok import tokenize


class TestAscii(unittest.TestCase):
    def test_words(self):
        toks = tokenize("ab cd")
        self.assertEqual([t.text for t in toks], ["ab", "cd"])
        self.assertEqual([(t.start, t.end) for t in toks], [(0, 2), (3, 5)])

    def test_byte_offsets_match_on_ascii(self):
        toks = tokenize("ab cd")
        self.assertEqual([(t.bstart, t.bend) for t in toks], [(0, 2), (3, 5)])

    def test_empty(self):
        self.assertEqual(tokenize(""), [])

    def test_only_whitespace(self):
        self.assertEqual(tokenize("   "), [])

    def test_slices_reproduce_the_text(self):
        s = "ab cd"
        for t in tokenize(s):
            self.assertEqual(s[t.start:t.end], t.text)


if __name__ == "__main__":
    unittest.main()
