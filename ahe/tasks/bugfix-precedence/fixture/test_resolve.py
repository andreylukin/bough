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


class TestScalars(unittest.TestCase):
    def test_higher_source_wins(self):
        c = Config(layers(defaults={"port": 80}, env={"port": 8080}))
        self.assertEqual(c.get("port"), 8080)

    def test_single_source(self):
        c = Config(layers(file={"host": "a"}))
        self.assertEqual(c.get("host"), "a")

    def test_missing_key(self):
        c = Config(layers(defaults={"port": 80}))
        with self.assertRaises(KeyError):
            c.get("nope")

    def test_pinned_beats_higher_source(self):
        c = Config(layers(file={"port": Value(80, pinned=True)}, flags={"port": 8080}))
        self.assertEqual(c.get("port"), 80)


class TestLists(unittest.TestCase):
    def test_single_source_list(self):
        c = Config(layers(file={"tags": ["a", "b"]}))
        self.assertEqual(c.get("tags"), ["a", "b"])

    def test_merge_is_lowest_source_first(self):
        c = Config(layers(defaults={"tags": ["a"]}, env={"tags": ["b"]}))
        self.assertEqual(c.get("tags"), ["a", "b"])

    def test_duplicate_keeps_first_occurrence(self):
        c = Config(layers(defaults={"tags": ["a", "b"]}, flags={"tags": ["b", "c"]}))
        self.assertEqual(c.get("tags"), ["a", "b", "c"])


class TestAliases(unittest.TestCase):
    def test_a_layer_renames_its_own_key(self):
        c = Config(layers(file={"bind_port": 80, "__aliases__": {"bind_port": "port"}}))
        self.assertEqual(c.get("port"), 80)

    def test_a_layer_without_aliases_is_untouched(self):
        c = Config(layers(env={"port": 8080}))
        self.assertEqual(c.get("port"), 8080)


class TestExplain(unittest.TestCase):
    def test_explain_names_the_key(self):
        c = Config(layers(env={"port": 8080}))
        self.assertIn("port", c.explain("port"))

    def test_explain_matches_a_simple_merge(self):
        c = Config(layers(defaults={"tags": ["a"]}, env={"tags": ["b"]}))
        self.assertIn(repr(c.get("tags")), c.explain("tags"))


if __name__ == "__main__":
    unittest.main()
