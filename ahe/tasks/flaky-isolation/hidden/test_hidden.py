"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from regs import PluginError, Registry


class TestIsolation(unittest.TestCase):
    """R1: registries do not see each other."""

    def test_a_fresh_registry_is_empty(self):
        first = Registry()
        first.register("alpha", lambda: 1)
        self.assertEqual(Registry().names(), [])

    def test_the_same_name_in_two_registries_is_fine(self):
        a, b = Registry(), Registry()
        a.register("dup", lambda: 1)
        b.register("dup", lambda: 2)
        self.assertEqual(a.get("dup")(), 1)
        self.assertEqual(b.get("dup")(), 2)

    def test_get_does_not_reach_into_another_registry(self):
        a, b = Registry(), Registry()
        a.register("only_in_a", lambda: 1)
        with self.assertRaises(PluginError):
            b.get("only_in_a")


class TestDuplicates(unittest.TestCase):
    """R2: still rejected within one registry."""

    def test_duplicate_within_one_registry(self):
        r = Registry()
        r.register("x", lambda: 1)
        with self.assertRaises(PluginError):
            r.register("x", lambda: 2)


class TestOptions(unittest.TestCase):
    """R3: no shared mutable default."""

    def test_options_do_not_leak_forward(self):
        Registry({"leaked": True})
        self.assertEqual(Registry().options(), {})

    def test_mutating_one_does_not_touch_another(self):
        a = Registry({"k": 1})
        b = Registry()
        a.options()["k"] = 99
        self.assertEqual(b.options(), {})
        self.assertEqual(Registry({"k": 1}).options(), {"k": 1})


class TestTagCache(unittest.TestCase):
    """R4: the cache is per-registry and invalidated on registration."""

    def test_cache_is_not_shared(self):
        a, b = Registry(), Registry()
        a.register("p", lambda: 1, tags=["t"])
        self.assertEqual(a.by_tag("t"), ["p"])
        self.assertEqual(b.by_tag("t"), [])

    def test_cache_sees_a_later_registration(self):
        r = Registry()
        self.assertEqual(r.by_tag("t"), [])
        r.register("p", lambda: 1, tags=["t"])
        self.assertEqual(r.by_tag("t"), ["p"])


if __name__ == "__main__":
    unittest.main()
