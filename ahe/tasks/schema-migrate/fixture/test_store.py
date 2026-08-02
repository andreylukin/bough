"""The checked-in suite. Covers the cases that already work."""

import unittest

from store import migrate


class TestUpgrade(unittest.TestCase):
    def test_v1_to_latest(self):
        self.assertEqual(
            migrate({"v": 1, "name": "Ada Lovelace", "email": "a@b.c"}),
            {"v": 4, "first": "Ada", "last": "Lovelace",
             "emails": ["a@b.c"], "active": True},
        )

    def test_v3_to_v4(self):
        self.assertEqual(
            migrate({"v": 3, "first": "Ada", "last": "L", "emails": ["a@b.c"]}),
            {"v": 4, "first": "Ada", "last": "L",
             "emails": ["a@b.c"], "active": True},
        )

    def test_partial_target(self):
        self.assertEqual(
            migrate({"v": 1, "name": "Ada Lovelace", "email": "a@b.c"}, to=2),
            {"v": 2, "first": "Ada", "last": "Lovelace", "email": "a@b.c"},
        )


if __name__ == "__main__":
    unittest.main()
