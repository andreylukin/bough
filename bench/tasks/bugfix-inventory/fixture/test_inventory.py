import unittest

from inventory import apply_movements, low_stock


class TestApplyMovements(unittest.TestCase):
    def test_applies_deltas(self):
        self.assertEqual(apply_movements({"a": 2}, [("a", 3), ("b", 1)]), {"a": 5, "b": 1})

    def test_input_not_mutated(self):
        stock = {"a": 2}
        apply_movements(stock, [("a", 1)])
        self.assertEqual(stock, {"a": 2})

    def test_rejects_negative(self):
        with self.assertRaises(ValueError):
            apply_movements({"a": 1}, [("a", -2)])


class TestLowStock(unittest.TestCase):
    def test_strictly_below(self):
        self.assertEqual(low_stock({"a": 1, "b": 3, "c": 2}, 2), ["a"])

    def test_at_threshold_is_not_low(self):
        self.assertEqual(low_stock({"a": 2}, 2), [])


if __name__ == "__main__":
    unittest.main()
