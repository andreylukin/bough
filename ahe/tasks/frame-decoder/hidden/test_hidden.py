"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from wire import Decoder, ProtocolError


def frame(payload):
    return len(payload).to_bytes(4, "big") + payload


class TestZeroLength(unittest.TestCase):
    """R1: a zero-length frame is a frame."""

    def test_alone(self):
        self.assertEqual(Decoder().feed(frame(b"")), [b""])

    def test_between_two_others(self):
        d = Decoder()
        self.assertEqual(
            d.feed(frame(b"a") + frame(b"") + frame(b"b")), [b"a", b"", b"b"]
        )

    def test_run_of_them(self):
        self.assertEqual(Decoder().feed(frame(b"") * 3), [b"", b"", b""])


class TestSplits(unittest.TestCase):
    """R2: byte-at-a-time is identical to all-at-once."""

    def test_byte_at_a_time(self):
        stream = frame(b"alpha") + frame(b"") + frame(b"beta") + frame(b"x" * 300)
        whole = Decoder().feed(stream)
        d = Decoder()
        drip = []
        for i in range(len(stream)):
            drip.extend(d.feed(stream[i : i + 1]))
        self.assertEqual(drip, whole)
        self.assertEqual(d.pending(), 0)

    def test_header_split_across_chunks(self):
        d = Decoder()
        f = frame(b"hello")
        self.assertEqual(d.feed(f[:2]), [])
        self.assertEqual(d.pending(), 2)
        self.assertEqual(d.feed(f[2:]), [b"hello"])

    def test_exactly_on_the_header_boundary(self):
        d = Decoder()
        f = frame(b"hello")
        self.assertEqual(d.feed(f[:4]), [])
        self.assertEqual(d.feed(f[4:]), [b"hello"])


class TestOversized(unittest.TestCase):
    """R3 + R4: rejected on the header, and fatal."""

    def test_rejected_on_the_header_alone(self):
        d = Decoder(max_frame=16)
        header = (1 << 30).to_bytes(4, "big")
        with self.assertRaises(ProtocolError):
            d.feed(header)

    def test_not_buffered_first(self):
        d = Decoder(max_frame=16)
        with self.assertRaises(ProtocolError):
            d.feed((99).to_bytes(4, "big") + b"x" * 10)

    def test_at_the_limit_is_fine(self):
        d = Decoder(max_frame=16)
        self.assertEqual(d.feed(frame(b"x" * 16)), [b"x" * 16])

    def test_the_decoder_is_poisoned(self):
        d = Decoder(max_frame=16)
        with self.assertRaises(ProtocolError):
            d.feed((99).to_bytes(4, "big"))
        with self.assertRaises(ProtocolError):
            d.feed(frame(b"fine"))
        with self.assertRaises(ProtocolError):
            d.pending()


if __name__ == "__main__":
    unittest.main()
