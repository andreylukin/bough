import unittest

from models import Account, Report
from bank import net_worth
from audit import is_solvent


class T(unittest.TestCase):
    def test_net_worth(self):
        self.assertEqual(net_worth([Account(10), Account(5)]), 15)

    def test_solvent(self):
        self.assertTrue(is_solvent(Account(0)))
        self.assertFalse(is_solvent(Account(-1)))

    def test_report_decoy_unchanged(self):
        r = Report()
        self.assertEqual(r.render(), "balance: 0")
        self.assertIn("balance", r.rows)


if __name__ == "__main__":
    unittest.main()
