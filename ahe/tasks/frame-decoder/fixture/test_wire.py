"""The checked-in suite. Covers the cases that already work."""

import unittest

from wire import Decoder


def frame(payload):
    return len(payload).to_bytes(4, "big") + payload


class TestFraming(unittest.TestCase):
    def test_one_whole_frame(self):
        d = Decoder()
        self.assertEqual(d.feed(frame(b"hello")), [b"hello"])

    def test_two_frames_in_one_chunk(self):
        d = Decoder()
        self.assertEqual(d.feed(frame(b"ab") + frame(b"cd")), [b"ab", b"cd"])

    def test_payload_split_across_chunks(self):
        d = Decoder()
        self.assertEqual(d.feed(frame(b"hello")[:6]), [])
        self.assertEqual(d.feed(frame(b"hello")[6:]), [b"hello"])

    def test_pending_counts_the_partial_tail(self):
        d = Decoder()
        d.feed(frame(b"hello")[:6])
        self.assertEqual(d.pending(), 6)

    def test_nothing_from_an_empty_chunk(self):
        d = Decoder()
        self.assertEqual(d.feed(b""), [])


if __name__ == "__main__":
    unittest.main()
