import unittest

from dispatcher import DispatchError
from machine import run


class TestBasicFlow(unittest.TestCase):
    def test_assign_resolve_confirm(self):
        self.assertEqual(run(["assign", "resolve", "confirm"])["state"], "closed")

    def test_escalation_is_recorded(self):
        ctx = run(["assign", "escalate"])
        self.assertEqual(ctx["state"], "escalated")
        self.assertTrue(ctx["escalated"])

    def test_unknown_transition_raises(self):
        with self.assertRaises(DispatchError):
            run(["assign", "approve"])


class TestReopen(unittest.TestCase):
    def test_reply_reopens_solved_ticket(self):
        ctx = run(["assign", "resolve", "customer_reply"])
        self.assertEqual(ctx["state"], "open")
        self.assertEqual(ctx["reopens"], 1)

    def test_third_reply_autocloses(self):
        events = [
            "assign",
            "resolve",
            "customer_reply",
            "resolve",
            "customer_reply",
            "resolve",
            "customer_reply",
        ]
        ctx = run(events)
        self.assertEqual(ctx["state"], "closed")
        self.assertEqual(ctx["reopens"], 2)


if __name__ == "__main__":
    unittest.main()
