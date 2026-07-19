import unittest

from cart import cart_total
from invoice import invoice_line


class TestPricing(unittest.TestCase):
    def test_cart_total(self):
        self.assertEqual(cart_total([("apple", 2.0, 3)]), 6.6)

    def test_invoice_line(self):
        self.assertEqual(invoice_line([("pen", 1.5, 2)], 0.0), "TOTAL: 3.00")


if __name__ == "__main__":
    unittest.main()
