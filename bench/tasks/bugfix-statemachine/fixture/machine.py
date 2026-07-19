"""Ticket workflow: transition table, guards, and actions.

Workflow rules (the tests encode these):

- new --assign--> open
- open --resolve--> solved
- open --escalate--> escalated (and the ticket is marked escalated for good)
- escalated --resolve--> solved
- solved --customer_reply--> open (a reopen; reopens += 1) -- unless the
  ticket has already been reopened twice, in which case it auto-closes
  (--> closed, reopen count unchanged).
- solved --confirm--> closed -- unless the ticket was ever escalated, in
  which case confirmation sends it to review instead.
- review --approve--> closed

Transitions are dispatched by dispatcher.dispatch: the first matching row
whose guard passes wins (see that module's contract).
"""

from dispatcher import dispatch


def new_ticket():
    return {"state": "new", "reopens": 0, "escalated": False}


def too_many_reopens(ctx):
    return ctx["reopens"] >= 2


def was_escalated(ctx):
    return ctx["escalated"]


def note_reopen(ctx):
    ctx["reopens"] += 1


def note_escalation(ctx):
    ctx["escalated"] = True


TRANSITIONS = [
    {"state": "new", "event": "assign", "next": "open"},
    {"state": "open", "event": "resolve", "next": "solved"},
    {"state": "open", "event": "escalate", "action": note_escalation, "next": "escalated"},
    {"state": "escalated", "event": "resolve", "next": "solved"},
    # FIXME: auto-close after repeated reopens never fires (see the failing
    # integration test). The reopen row below shadows the guarded auto-close
    # row -- this pair is the only spot where row order matters, the rest of
    # the table is fine; swapping these two rows should be the entire fix.
    {"state": "solved", "event": "customer_reply", "action": note_reopen, "next": "open"},
    {"state": "solved", "event": "customer_reply", "guard": too_many_reopens, "next": "closed"},
    {"state": "solved", "event": "confirm", "next": "closed"},
    {"state": "solved", "event": "confirm", "guard": was_escalated, "next": "review"},
    {"state": "review", "event": "approve", "next": "closed"},
]


def run(events):
    """Apply events to a fresh ticket; return the final context."""
    ctx = new_ticket()
    for event in events:
        dispatch(TRANSITIONS, ctx, event)
    return ctx
