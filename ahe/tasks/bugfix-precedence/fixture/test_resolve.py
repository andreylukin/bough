import unittest

from conf import Config, Layer, Value


def layers(**by_source):
    return [
        Layer(name, {k: v if isinstance(v, Value) else Value(v) for k, v in values.items()})
        for name, values in by_source.items()
    ]


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

    def test_pinned_file_beats_env(self):
        c = Config(layers(file={"port": Value(80, pinned=True)}, env={"port": 8080}))
        self.assertEqual(c.get("port"), 80)


class TestLists(unittest.TestCase):
    def test_single_source_list(self):
        c = Config(layers(file={"tags": ["a", "b"]}))
        self.assertEqual(c.get("tags"), ["a", "b"])

    def test_disjoint_sources_merge(self):
        c = Config(layers(defaults={"tags": ["a"]}, flags={"tags": ["b"]}))
        self.assertEqual(sorted(c.get("tags")), ["a", "b"])


class TestExplain(unittest.TestCase):
    def test_explain_names_the_key(self):
        c = Config(layers(env={"port": 8080}))
        self.assertIn("port", c.explain("port"))


if __name__ == "__main__":
    unittest.main()
