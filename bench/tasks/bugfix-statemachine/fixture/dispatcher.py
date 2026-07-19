"""Generic table-driven event dispatcher.

Contract: transition rows are tried strictly in table order; the first row
whose state and event match AND whose guard passes (a missing guard always
passes) is taken -- its action (if any) runs, then the state advances. Rows
after the first match are never consulted.

This module is stable and workflow-agnostic; workflow changes belong in the
transition table that is passed in.
"""


class DispatchError(Exception):
    pass


def dispatch(table, ctx, event):
    """Apply one event to ctx; return the new state."""
    for row in table:
        if row["state"] != ctx["state"] or row["event"] != event:
            continue
        guard = row.get("guard")
        if guard is not None and not guard(ctx):
            continue
        action = row.get("action")
        if action is not None:
            action(ctx)
        ctx["state"] = row["next"]
        return ctx["state"]
    raise DispatchError(f"no transition from {ctx['state']!r} on {event!r}")
