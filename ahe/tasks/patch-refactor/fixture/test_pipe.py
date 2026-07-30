import unittest

from pipe.legacy import call_legacy, legacy_request
from pipe.run import run


class TestChain(unittest.TestCase):
    def test_private_path_gets_a_user(self):
        out = run(legacy_request("/orders/17"))
        self.assertEqual(call_body(out), "GET /orders/17 by u-orders")

    def test_public_path_has_no_user(self):
        out = run(legacy_request("/public/health"))
        self.assertEqual(call_body(out), "GET /public/health by None")

    def test_delete_without_a_user_is_forbidden(self):
        out = run(legacy_request("/public/thing", "DELETE"))
        self.assertEqual(status(out), 403)
        self.assertEqual(call_body(out), "forbidden")

    def test_trace_is_set(self):
        out = run(legacy_request("/orders/17"))
        self.assertEqual(field(out, "trace"), "GET:/orders/17")


class TestLegacyCaller(unittest.TestCase):
    def test_legacy_caller_still_works(self):
        self.assertEqual(call_legacy(run, "/orders/9"), "GET /orders/9 by u-orders")


def call_body(out):
    return field(out, "body")


def status(out):
    return field(out, "status")


def field(out, name):
    return getattr(out, name) if hasattr(out, name) else out[name]


if __name__ == "__main__":
    unittest.main()
