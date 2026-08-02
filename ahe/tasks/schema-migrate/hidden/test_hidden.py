"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from store import MigrationError, migrate

V1 = {"v": 1, "name": "Ada Lovelace", "email": "a@b.c"}


class TestNoMutation(unittest.TestCase):
    """R1: nothing shared with the caller, at any depth."""

    def test_input_is_unchanged(self):
        original = dict(V1)
        migrate(V1)
        self.assertEqual(V1, original)

    def test_lists_are_not_shared(self):
        src = {"v": 3, "first": "A", "last": "B", "emails": ["a@b.c"]}
        out = migrate(src)
        out["emails"].append("x@y.z")
        self.assertEqual(src["emails"], ["a@b.c"])

    def test_same_version_returns_a_copy(self):
        src = {"v": 3, "first": "A", "last": "B", "emails": ["a@b.c"]}
        out = migrate(src, to=3)
        self.assertEqual(out, src)
        self.assertIsNot(out, src)
        self.assertIsNot(out["emails"], src["emails"])


class TestNameSplitting(unittest.TestCase):
    """R2: split on the LAST space, and never raise."""

    def cases(self):
        return [
            ("Ada Lovelace", "Ada", "Lovelace"),
            ("Ada King Lovelace", "Ada King", "Lovelace"),
            ("Cher", "", "Cher"),
            ("", "", ""),
            ("a b c d", "a b c", "d"),
        ]

    def test_splits(self):
        for name, first, last in self.cases():
            with self.subTest(name):
                out = migrate({"v": 1, "name": name, "email": "e"}, to=2)
                self.assertEqual((out["first"], out["last"]), (first, last))


class TestDowngrade(unittest.TestCase):
    """R3: the steps reverse."""

    def test_v4_to_v3_drops_active(self):
        out = migrate(migrate(V1), to=3)
        self.assertEqual(
            out, {"v": 3, "first": "Ada", "last": "Lovelace", "emails": ["a@b.c"]}
        )

    def test_v3_to_v2_keeps_the_primary_and_drops_the_rest(self):
        src = {"v": 3, "first": "A", "last": "B", "emails": ["p@x", "s@x"]}
        self.assertEqual(
            migrate(src, to=2), {"v": 2, "first": "A", "last": "B", "email": "p@x"}
        )

    def test_v3_to_v2_with_no_emails_raises(self):
        with self.assertRaises(MigrationError):
            migrate({"v": 3, "first": "A", "last": "B", "emails": []}, to=2)

    def test_all_the_way_down(self):
        self.assertEqual(migrate(migrate(V1), to=1), V1)

    def test_name_rejoin_has_no_stray_spaces(self):
        out = migrate({"v": 2, "first": "", "last": "Cher", "email": "e"}, to=1)
        self.assertEqual(out["name"], "Cher")
        out = migrate({"v": 2, "first": "", "last": "", "email": "e"}, to=1)
        self.assertEqual(out["name"], "")


class TestRoundTrips(unittest.TestCase):
    """R4 + R5: stable in both directions."""

    def test_every_pair(self):
        for lo in range(1, 5):
            base = migrate(V1, to=lo)
            for hi in range(1, 5):
                with self.subTest(lo=lo, hi=hi):
                    there = migrate(base, to=hi)
                    self.assertEqual(migrate(there, to=lo), base)

    def test_idempotent(self):
        once = migrate(V1)
        self.assertEqual(migrate(once), once)


class TestBadInput(unittest.TestCase):
    """R6: MigrationError, not KeyError or AttributeError."""

    def test_no_version(self):
        with self.assertRaises(MigrationError):
            migrate({"name": "Ada Lovelace"})

    def test_version_out_of_range(self):
        for v in (0, 5, 99, "3"):
            with self.subTest(v=v):
                with self.assertRaises(MigrationError):
                    migrate({"v": v})

    def test_target_out_of_range(self):
        for to in (0, 5, "2"):
            with self.subTest(to=to):
                with self.assertRaises(MigrationError):
                    migrate(V1, to=to)


if __name__ == "__main__":
    unittest.main()
