"""The checked-in suite. Reaches PathResolver only indirectly, so it survives a
rename of its method."""

import unittest

from svc.index import Index
from svc.loader import Loader
from svc.paths import PathResolver
from svc.report import render


class TestBehaviour(unittest.TestCase):
    def test_resolve_all(self):
        self.assertEqual(
            PathResolver("/root").resolve_all(["a", "/b"]), ["/root/a", "/b"]
        )

    def test_loader(self):
        loader = Loader("/root", {"/root/a": "A"})
        self.assertEqual(loader.load("a"), "A")
        self.assertEqual(loader.load_many(["a"]), ["A"])

    def test_index(self):
        idx = Index("/root", {"h": "10.0.0.1"})
        entry = idx.entry("a", "h")
        self.assertEqual(entry["path"], "/root/a")
        self.assertEqual(entry["addr"], "10.0.0.1")
        self.assertEqual(idx.batch(["a", "b"]), ["/root/a", "/root/b"])

    def test_render(self):
        self.assertEqual(render("/root", ["a"]), "resolve(a) -> /root/a")


if __name__ == "__main__":
    unittest.main()
