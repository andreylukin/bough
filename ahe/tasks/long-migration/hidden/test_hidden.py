"""The grading suite. Never present in the workspace; copied in by verify.sh.

Every one of the twenty is checked individually, so a partial migration reads as
exactly the modules it missed.
"""

import unittest

import svc.ops as ops
from svc.result import Result

# (name, good_args, bad_args, expected_value, expected_error)
CASES = [
    ("parse_int", ("7",), ("x",), 7, "not an integer"),
    ("halve", (8,), ("x",), 4, "odd number"),
    ("first", ([1, 2],), ([],), 1, "empty sequence"),
    ("lookup", ({"k": 5},), ({},), 5, "missing key"),
    ("invert", (2,), (0,), 0.5, "division by zero"),
    ("upper", ("ab",), (None,), "AB", "not text"),
    ("last", ([1, 2],), ([],), 2, "empty sequence"),
    ("length", ([1, 2],), (7,), 2, "no length"),
    ("negate", (3,), ("x",), -3, "not a number"),
    ("join_all", (["a", "b"],), ([1],), "a,b", "not all strings"),
    ("square", (3,), ("x",), 9, "not a number"),
    ("strip", ("  a ",), (None,), "a", "not text"),
    ("to_float", ("1.5",), ("x",), 1.5, "not a float"),
    ("keys", ({"b": 1, "a": 2},), (None,), ["a", "b"], "not a mapping"),
    ("values", ({"b": 1, "a": 2},), (None,), [1, 2], "not a mapping"),
    ("head_two", ([1, 2, 3],), (None,), [1, 2], "not a sequence"),
    ("count_a", ("banana",), (None,), 3, "not text"),
    ("as_bool", (1,), (), True, "not truthy-able"),
    ("repeat", ("ab",), (None,), "abab", "not repeatable"),
    ("sum_all", ([1, 2],), (["a"],), 3, "not summable"),
]


class TestEveryModuleMigrated(unittest.TestCase):
    def test_all_twenty_return_results(self):
        missed = []
        for name, good, _bad, _v, _e in CASES:
            out = getattr(ops, name)(*good)
            if not isinstance(out, Result):
                missed.append(name)
        self.assertEqual(missed, [], f"still returning tuples: {missed}")

    def test_success_values_unchanged(self):
        for name, good, _bad, value, _e in CASES:
            with self.subTest(name):
                out = getattr(ops, name)(*good)
                self.assertTrue(out.ok)
                self.assertEqual(out.value, value)
                self.assertEqual(out.unwrap(), value)

    def test_error_messages_unchanged(self):
        for name, _good, bad, _v, error in CASES:
            if not bad:
                continue
            with self.subTest(name):
                out = getattr(ops, name)(*bad)
                self.assertFalse(out.ok, f"{name} did not fail")
                self.assertEqual(out.error, error)

    def test_wrappers_still_work(self):
        for name, good, bad, value, _e in CASES:
            with self.subTest(name):
                wrapper = getattr(ops, f"{name}_or")
                self.assertEqual(wrapper("D", *good), value)
                if bad:
                    self.assertEqual(wrapper("D", *bad), "D")

    def test_all_forty_names_exported(self):
        for name, *_ in CASES:
            self.assertTrue(hasattr(ops, name), name)
            self.assertTrue(hasattr(ops, f"{name}_or"), f"{name}_or")


if __name__ == "__main__":
    unittest.main()
