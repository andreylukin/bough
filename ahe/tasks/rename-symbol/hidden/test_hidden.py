"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from svc import dns, index, report
from svc.index import Index
from svc.loader import Loader
from svc.paths import PathResolver
from svc.report import render


class TestTheRenameHappened(unittest.TestCase):
    def test_new_name_exists_and_works(self):
        self.assertEqual(PathResolver("/root").resolve_path("a"), "/root/a")
        self.assertEqual(PathResolver("/root").resolve_path("/b"), "/b")

    def test_old_name_is_gone(self):
        self.assertFalse(
            hasattr(PathResolver("/root"), "resolve"),
            "PathResolver still has a `resolve`",
        )


class TestTheDecoysSurvived(unittest.TestCase):
    def test_name_resolver_is_untouched(self):
        r = dns.NameResolver({"h": "10.0.0.1"})
        self.assertEqual(r.resolve("h"), "10.0.0.1")
        self.assertEqual(r.resolve("nope"), "0.0.0.0")
        self.assertFalse(hasattr(r, "resolve_path"), "NameResolver was renamed too")

    def test_module_level_helper_is_untouched(self):
        self.assertEqual(report.resolve("  x  "), "x")
        self.assertFalse(
            hasattr(report, "resolve_path"), "report.resolve was renamed too"
        )

    def test_user_visible_strings_are_untouched(self):
        self.assertEqual(index.HELP, "resolve: map a ref to a path")
        self.assertEqual(report.TEMPLATE, "resolve({ref}) -> {path}")


class TestBehaviourUnchanged(unittest.TestCase):
    def test_call_sites_all_still_work(self):
        self.assertEqual(
            PathResolver("/root").resolve_all(["a", "/b"]), ["/root/a", "/b"]
        )
        self.assertEqual(Loader("/root", {"/root/a": "A"}).load("a"), "A")
        idx = Index("/root", {"h": "10.0.0.1"})
        self.assertEqual(idx.entry("a", "h"), {
            "path": "/root/a", "addr": "10.0.0.1", "help": index.HELP,
        })
        self.assertEqual(idx.batch(["a"]), ["/root/a"])
        self.assertEqual(render("/root", ["a"]), "resolve(a) -> /root/a")


if __name__ == "__main__":
    unittest.main()
