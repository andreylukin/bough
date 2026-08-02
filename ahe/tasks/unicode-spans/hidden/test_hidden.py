"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from tok import tokenize


class TestClassRuns(unittest.TestCase):
    """R1: a class change ends a token; punctuation is one char each."""

    def test_punctuation_splits_words(self):
        toks = tokenize("hello,world")
        self.assertEqual([(t.kind, t.text) for t in toks],
                         [("word", "hello"), ("punct", ","), ("word", "world")])

    def test_letters_and_digits_split(self):
        toks = tokenize("abc123")
        self.assertEqual([(t.kind, t.text) for t in toks],
                         [("word", "abc"), ("number", "123")])

    def test_each_punct_is_its_own_token(self):
        self.assertEqual([t.text for t in tokenize("?!")], ["?", "!"])

    def test_mixed(self):
        self.assertEqual(
            [t.text for t in tokenize("a1 b, 22c")],
            ["a", "1", "b", ",", "22", "c"],
        )


class TestCodePointOffsets(unittest.TestCase):
    """R3: text[start:end] round-trips."""

    def test_ascii(self):
        s = "hi, there 42"
        for t in tokenize(s):
            self.assertEqual(s[t.start:t.end], t.text)

    def test_non_ascii(self):
        s = "naïve café 42 — ok"
        for t in tokenize(s):
            self.assertEqual(s[t.start:t.end], t.text)

    def test_astral(self):
        s = "a 🙂 b"
        for t in tokenize(s):
            self.assertEqual(s[t.start:t.end], t.text)


class TestByteOffsets(unittest.TestCase):
    """R4: the byte view round-trips too, and differs from the code-point view."""

    def test_bytes_round_trip(self):
        for s in ["naïve café", "a 🙂 b", "αβγ 123", "éclair", "日本語 x"]:
            raw = s.encode("utf-8")
            for t in tokenize(s):
                self.assertEqual(raw[t.bstart:t.bend].decode("utf-8"), t.text, s)

    def test_byte_and_codepoint_offsets_actually_diverge(self):
        toks = tokenize("é x")
        self.assertEqual((toks[1].start, toks[1].end), (2, 3))
        self.assertEqual((toks[1].bstart, toks[1].bend), (3, 4))

    def test_astral_is_four_bytes_one_codepoint(self):
        toks = tokenize("🙂")
        self.assertEqual((toks[0].start, toks[0].end), (0, 1))
        self.assertEqual((toks[0].bstart, toks[0].bend), (0, 4))


class TestUnicodeClasses(unittest.TestCase):
    """R5: non-ASCII letters are letters; unicode whitespace separates."""

    def test_greek_is_a_word(self):
        toks = tokenize("αβγ")
        self.assertEqual([(t.kind, t.text) for t in toks], [("word", "αβγ")])

    def test_non_breaking_space_separates(self):
        self.assertEqual([t.text for t in tokenize("a b")], ["a", "b"])

    def test_combining_mark_stays_in_the_word(self):
        s = "éclair"
        toks = tokenize(s)
        self.assertEqual(len(toks), 1)
        self.assertEqual(toks[0].text, s)


if __name__ == "__main__":
    unittest.main()
