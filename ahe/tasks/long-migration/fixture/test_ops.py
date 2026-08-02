"""The checked-in suite. It uses the `_or` wrappers only, so it survives the
migration of the return type underneath them."""

import unittest

import svc.ops as ops


class TestWrappers(unittest.TestCase):
    def test_success_paths(self):
        self.assertEqual(ops.parse_int_or(-1, "7"), 7)
        self.assertEqual(ops.upper_or("", "ab"), "AB")
        self.assertEqual(ops.sum_all_or(-1, [1, 2, 3]), 6)

    def test_failure_paths(self):
        self.assertEqual(ops.parse_int_or(-1, "x"), -1)
        self.assertEqual(ops.first_or("none", []), "none")
        self.assertEqual(ops.invert_or(0.0, 0), 0.0)

    def test_every_module_is_importable(self):
        self.assertEqual(len([n for n in dir(ops) if n.endswith("_or")]), 20)


if __name__ == "__main__":
    unittest.main()
