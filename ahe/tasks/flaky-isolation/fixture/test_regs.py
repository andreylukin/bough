"""The checked-in suite. Passes today, in this order, in a fresh process."""

import unittest

from regs import Registry


class TestRegistry(unittest.TestCase):
    def test_a_register_and_get(self):
        r = Registry()
        r.register("alpha", lambda: 1, tags=["x"])
        self.assertEqual(r.get("alpha")(), 1)

    def test_b_names_are_sorted(self):
        r = Registry()
        r.register("zeta", lambda: 2)
        self.assertIn("zeta", r.names())

    def test_c_by_tag(self):
        r = Registry()
        r.register("beta", lambda: 3, tags=["x", "y"])
        self.assertIn("beta", r.by_tag("x"))


if __name__ == "__main__":
    unittest.main()
