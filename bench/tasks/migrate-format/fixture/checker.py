"""Round-trip acceptance checks for the v2 record format.

Not part of the regular suite (unittest discovery skips it); run explicitly:

    python3 -m unittest checker
"""

import os
import tempfile
import unittest

from recio.reader import read_records
from recio.writer import write_records

CASES = [
    [],
    [{"id": "a1", "name": "plain", "qty": "1", "note": ""}],
    [{"id": "a2", "name": "eq=in=name", "qty": "2", "note": "x=y"}],
    [{"id": "a3", "name": "multi\nline", "qty": "3", "note": "ends with newline\n"}],
    [{"id": "a4", "name": "back\\slash", "qty": "4", "note": "literal \\n stays two chars"}],
    [
        {"id": "a5", "name": "", "qty": "0", "note": "= starts the note"},
        {"id": "a6", "name": "blank\n\ninside", "qty": "6", "note": "\\"},
    ],
]


class TestRoundTrip(unittest.TestCase):
    def test_write_then_read_is_identity(self):
        for records in CASES:
            with tempfile.TemporaryDirectory() as d:
                path = os.path.join(d, "rt.dat")
                write_records(path, records)
                self.assertEqual(read_records(path), records)

    def test_writer_emits_v2_header(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "rt.dat")
            write_records(path, [{"id": "h", "name": "n", "qty": "1", "note": ""}])
            with open(path) as f:
                self.assertEqual(f.readline(), "#recio v2\n")


if __name__ == "__main__":
    unittest.main()
