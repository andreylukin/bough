"""The grading suite. Never present in the workspace; copied in by verify.sh.

Each test names the spec rule it enforces, so a failure reads as a spec violation
rather than as a diff against an implementation.
"""

import unittest

from conf import Config, Layer, Value


def layers(**by_source):
    out = []
    for name, values in by_source.items():
        aliases = values.pop("__aliases__", {})
        out.append(
            Layer(
                name,
                {k: v if isinstance(v, Value) else Value(v) for k, v in values.items()},
                aliases,
            )
        )
    return out


class TestAliasesAreGlobal(unittest.TestCase):
    """Spec R1: the resolved alias map applies to EVERY source's keys."""

    def test_alias_declared_high_applies_to_a_lower_source(self):
        c = Config(
            layers(
                defaults={"bind_port": 80},
                flags={"__aliases__": {"bind_port": "port"}},
            )
        )
        self.assertEqual(c.get("port"), 80)

    def test_alias_declared_low_applies_to_a_higher_source(self):
        c = Config(
            layers(
                defaults={"__aliases__": {"bind_port": "port"}},
                flags={"bind_port": 9090},
            )
        )
        self.assertEqual(c.get("port"), 9090)

    def test_aliased_and_canonical_claims_unify_under_precedence(self):
        c = Config(
            layers(
                defaults={"bind_port": 80},
                file={"__aliases__": {"bind_port": "port"}},
                env={"port": 8080},
            )
        )
        self.assertEqual(c.get("port"), 8080)

    def test_conflicting_aliases_resolve_by_precedence(self):
        c = Config(
            layers(
                defaults={"__aliases__": {"tag": "labels"}},
                flags={"__aliases__": {"tag": "tags"}},
                file={"tag": ["x"]},
            )
        )
        self.assertEqual(c.get("tags"), ["x"])
        with self.assertRaises(KeyError):
            c.get("labels")

    def test_aliased_lists_merge_after_renaming(self):
        c = Config(
            layers(
                defaults={"tag": ["a"], "__aliases__": {"tag": "tags"}},
                env={"tags": ["b"]},
            )
        )
        self.assertEqual(c.get("tags"), ["a", "b"])


class TestPinnedListTruncates(unittest.TestCase):
    """Spec R4: sources strictly below the highest pinned list contribute nothing."""

    def test_pinned_list_drops_lower_sources(self):
        c = Config(
            layers(
                defaults={"tags": ["a"]},
                env={"tags": Value(["b"], pinned=True)},
                flags={"tags": ["c"]},
            )
        )
        self.assertEqual(c.get("tags"), ["b", "c"])

    def test_highest_pinned_list_is_the_cut(self):
        c = Config(
            layers(
                defaults={"tags": Value(["a"], pinned=True)},
                file={"tags": ["b"]},
                env={"tags": Value(["c"], pinned=True)},
                flags={"tags": ["d"]},
            )
        )
        self.assertEqual(c.get("tags"), ["c", "d"])

    def test_unpinned_lists_still_merge_whole(self):
        c = Config(layers(defaults={"tags": ["a"]}, env={"tags": ["b"]}, flags={"tags": ["c"]}))
        self.assertEqual(c.get("tags"), ["a", "b", "c"])

    def test_scalar_pinning_is_unaffected(self):
        c = Config(layers(defaults={"port": Value(80, pinned=True)}, flags={"port": 9090}))
        self.assertEqual(c.get("port"), 80)


class TestExplainAgrees(unittest.TestCase):
    """Spec R5: explain() must report the outcome resolve() produces."""

    def test_explain_matches_a_truncated_merge(self):
        c = Config(
            layers(
                defaults={"tags": ["a"]},
                env={"tags": Value(["b"], pinned=True)},
                flags={"tags": ["c"]},
            )
        )
        self.assertIn(repr(c.get("tags")), c.explain("tags"))

    def test_explain_matches_an_aliased_resolution(self):
        c = Config(
            layers(
                defaults={"bind_port": 80},
                flags={"__aliases__": {"bind_port": "port"}},
            )
        )
        self.assertIn(repr(c.get("port")), c.explain("port"))

    def test_explain_missing_key_raises(self):
        c = Config(layers(defaults={"port": 80}))
        with self.assertRaises(KeyError):
            c.explain("nope")


if __name__ == "__main__":
    unittest.main()
