import os
import tempfile
import unittest

from recio.reader import read_records


def write_tmp(dirname, text):
    path = os.path.join(dirname, "records.dat")
    with open(path, "w") as f:
        f.write(text)
    return path


class TestV1Reading(unittest.TestCase):
    def test_reads_positional_fields(self):
        with tempfile.TemporaryDirectory() as d:
            path = write_tmp(d, "r1,alpha,4,fragile\n")
            self.assertEqual(
                read_records(path),
                [{"id": "r1", "name": "alpha", "qty": "4", "note": "fragile"}],
            )

    def test_note_keeps_commas_and_equals(self):
        with tempfile.TemporaryDirectory() as d:
            path = write_tmp(d, "r2,beta,2,size=XL, flat\n")
            self.assertEqual(read_records(path)[0]["note"], "size=XL, flat")

    def test_blank_lines_and_empty_note(self):
        with tempfile.TemporaryDirectory() as d:
            path = write_tmp(d, "\nr3,gamma,1,\n\n")
            recs = read_records(path)
            self.assertEqual(len(recs), 1)
            self.assertEqual(recs[0]["note"], "")


if __name__ == "__main__":
    unittest.main()
