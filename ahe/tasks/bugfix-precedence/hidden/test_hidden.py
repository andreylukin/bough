"""The grading suite. Never present in the workspace; copied in by verify.sh.

Each test names the spec rule it enforces, so a failure reads as a spec violation
rather than as a diff against an implementation.
"""

import unittest

from conf import Config, Layer, Value


def layers(**by_source):
    return [
        Layer(name, {k: v if isinstance(v, Value) else Value(v) for k, v in values.items()})
        for name, values in by_source.items()
    ]


class TestPinning(unittest.TestCase):
    """Spec R2: pinning applies to EVERY source, not to some of them."""

    def test_pinned_defaults_beat_env(self):
        c = Config(layers(defaults={"port": Value(80, pinned=True)}, env={"port": 8080}))
        self.assertEqual(c.get("port"), 80)

    def test_pinned_defaults_beat_flags(self):
        c = Config(layers(defaults={"port": Value(80, pinned=True)}, flags={"port": 9090}))
        self.assertEqual(c.get("port"), 80)

    def test_pinned_env_beats_flags(self):
        c = Config(layers(env={"port": Value(80, pinned=True)}, flags={"port": 9090}))
        self.assertEqual(c.get("port"), 80)

    def test_highest_pinned_wins_over_lower_pinned(self):
        c = Config(
            layers(
                defaults={"port": Value(80, pinned=True)},
                env={"port": Value(8080, pinned=True)},
            )
        )
        self.assertEqual(c.get("port"), 8080)

    def test_unpinned_still_follows_precedence(self):
        c = Config(layers(defaults={"port": 80}, flags={"port": 9090}))
        self.assertEqual(c.get("port"), 9090)


class TestListOrder(unittest.TestCase):
    """Spec R3: source order, lowest first, each value at its FIRST occurrence."""

    def test_merge_is_lowest_source_first(self):
        c = Config(layers(defaults={"tags": ["a"]}, env={"tags": ["b"]}, flags={"tags": ["c"]}))
        self.assertEqual(c.get("tags"), ["a", "b", "c"])

    def test_duplicate_keeps_first_occurrence_position(self):
        c = Config(layers(defaults={"tags": ["a", "b"]}, flags={"tags": ["b", "c"]}))
        self.assertEqual(c.get("tags"), ["a", "b", "c"])

    def test_duplicate_across_three_sources(self):
        c = Config(
            layers(
                defaults={"tags": ["x", "y"]},
                file={"tags": ["y", "z"]},
                env={"tags": ["z", "x", "w"]},
            )
        )
        self.assertEqual(c.get("tags"), ["x", "y", "z", "w"])


class TestExplainAgrees(unittest.TestCase):
    """Spec: explain() must report the outcome resolve() produces."""

    def test_explain_matches_resolved_list(self):
        c = Config(layers(defaults={"tags": ["a", "b"]}, flags={"tags": ["b", "c"]}))
        self.assertIn(repr(c.get("tags")), c.explain("tags"))

    def test_explain_matches_resolved_scalar_when_pinned_low(self):
        c = Config(layers(defaults={"port": Value(80, pinned=True)}, flags={"port": 9090}))
        self.assertIn(repr(c.get("port")), c.explain("port"))
        self.assertIn("defaults", c.explain("port"))

    def test_explain_missing_key_raises(self):
        c = Config(layers(defaults={"port": 80}))
        with self.assertRaises(KeyError):
            c.explain("nope")


if __name__ == "__main__":
    unittest.main()
