import unittest

from ledger import load_entries
from report import format_report, total_cents


class TestLedger(unittest.TestCase):
    def test_parses_cents_exactly(self):
        self.assertEqual(load_entries(["book 19.99"]), [("book", 1999)])

    def test_currency_markup(self):
        self.assertEqual(load_entries(["laptop $1,299.99"]), [("laptop", 129999)])


class TestReport(unittest.TestCase):
    def test_total_matches_bank_statement(self):
        entries = load_entries(["coffee 4.70", "book 19.99", "tip 0.10"])
        self.assertEqual(total_cents(entries), 2479)

    def test_refund_is_negative(self):
        entries = load_entries(["refund -0.07"])
        self.assertEqual(total_cents(entries), -7)

    def test_format_report(self):
        entries = load_entries(["coffee 4.70", "book 19.99"])
        self.assertEqual(format_report(entries), "coffee 470\nbook 1999\nTOTAL 2469")


if __name__ == "__main__":
    unittest.main()
