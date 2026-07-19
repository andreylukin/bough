import unittest

from audit import audit
from receiving import receive
from shipping import ship_manifest

RECORDS = [
    "W-1|4|dock A",
    "W-2|1|dock B",
    "",
    "W-1|2|dock A",
]


class TestReceive(unittest.TestCase):
    def test_totals_by_sku(self):
        self.assertEqual(receive(RECORDS), {"W-1": 6, "W-2": 1})

    def test_bad_record_raises(self):
        with self.assertRaises(ValueError):
            receive(["W-1|4"])


class TestShipManifest(unittest.TestCase):
    def test_manifest_lines(self):
        self.assertEqual(
            ship_manifest(RECORDS),
            ["W-1 x4 @ dock A", "W-2 x1 @ dock B", "W-1 x2 @ dock A"],
        )


class TestAudit(unittest.TestCase):
    def test_counts_by_location(self):
        self.assertEqual(audit(RECORDS), {"dock A": 2, "dock B": 1})


if __name__ == "__main__":
    unittest.main()
