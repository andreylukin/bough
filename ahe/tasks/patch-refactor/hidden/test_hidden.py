"""The grading suite. Never present in the workspace; copied in by verify.sh.

Structural checks first, then behaviour. A refactor that left the dict shape in
place is not a refactor, however green the behavioural tests are — and the
behavioural tests were written to pass on the dict version too, so on their own
they cannot tell the difference.
"""

import ast
import dataclasses
import inspect
import unittest
from pathlib import Path

from pipe.context import Ctx
from pipe.legacy import call_legacy, legacy_request
from pipe.run import run
from pipe.stages import CHAIN

PIPE = Path(__file__).resolve().parent / "pipe"


class TestTheDictIsGone(unittest.TestCase):
    """Requirement 1: stages take and return Ctx, and none of them subscripts it."""

    def test_no_stage_is_annotated_with_dict(self):
        for stage in CHAIN:
            hints = getattr(stage, "__annotations__", {})
            for name, ann in hints.items():
                text = str(ann)
                self.assertNotIn(
                    "ict", text, f"{stage.__name__} still annotates {name} as {text}"
                )

    def test_no_stage_subscripts_its_context(self):
        # `ctx["path"]` is the dict shape wearing a new type name.
        source = (PIPE / "stages.py").read_text()
        tree = ast.parse(source)
        for node in ast.walk(tree):
            if isinstance(node, ast.Subscript) and isinstance(node.value, ast.Name):
                self.fail(f"stages.py still subscripts {node.value.id} at line {node.lineno}")

    def test_stages_actually_return_a_ctx(self):
        out = run(Ctx(path="/orders/1", method="GET"))
        self.assertIsInstance(out, Ctx)


class TestFrozen(unittest.TestCase):
    """Requirement 2: nothing mutates a context in place."""

    def test_ctx_is_still_frozen(self):
        self.assertTrue(dataclasses.fields(Ctx) and Ctx.__dataclass_params__.frozen)

    def test_the_input_context_is_not_mutated(self):
        start = Ctx(path="/orders/1", method="GET")
        run(start)
        self.assertIsNone(start.user, "the caller's context was mutated in place")
        self.assertEqual(start.body, "")

    def test_no_stage_assigns_to_an_attribute_of_its_argument(self):
        tree = ast.parse((PIPE / "stages.py").read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                for t in node.targets:
                    if isinstance(t, ast.Attribute):
                        self.fail(f"stages.py assigns to an attribute at line {node.lineno}")


class TestBehaviourUnchanged(unittest.TestCase):
    """Requirement 3, including the paths the checked-in tests never touch."""

    def test_private_path(self):
        self.assertEqual(
            run(Ctx(path="/orders/17", method="GET")).body, "GET /orders/17 by u-orders"
        )

    def test_public_path(self):
        self.assertEqual(
            run(Ctx(path="/public/health", method="GET")).body,
            "GET /public/health by None",
        )

    def test_delete_forbidden(self):
        out = run(Ctx(path="/public/thing", method="DELETE"))
        self.assertEqual((out.status, out.body), (403, "forbidden"))

    def test_trace(self):
        self.assertEqual(run(Ctx(path="/a/b", method="PUT")).trace, "PUT:/a/b")

    def test_missing_field_now_raises_attribute_error(self):
        with self.assertRaises(AttributeError):
            getattr(Ctx(path="/a", method="GET"), "nope")

    def test_an_unset_optional_is_still_none_not_an_error(self):
        # The distinction requirement 3 names: absent-and-optional is None, but
        # absent-and-undeclared raises. A __getattr__ that returns None for
        # everything would satisfy the test above and break this one.
        self.assertIsNone(Ctx(path="/a", method="GET").user)
        self.assertIsNone(Ctx(path="/a", method="GET").trace)


class TestLegacyBoundary(unittest.TestCase):
    """Requirement 4: the vendored dict caller keeps working, unmodified."""

    def test_legacy_caller_still_works(self):
        self.assertEqual(call_legacy(run, "/orders/9"), "GET /orders/9 by u-orders")

    def test_entry_point_accepts_a_dict_and_returns_a_ctx(self):
        out = run(legacy_request("/orders/9"))
        self.assertIsInstance(out, Ctx, "the dict must be converted at the boundary")

    def test_conversion_happens_at_the_entry_point_not_in_the_stages(self):
        # If a stage does the converting, every stage has to keep handling both
        # shapes — which is the dict leaking downstream that requirement 4 forbids.
        for stage in CHAIN:
            with self.assertRaises((AttributeError, TypeError)):
                stage({"path": "/a", "method": "GET", "user": None})


if __name__ == "__main__":
    unittest.main()
