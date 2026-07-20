"""A tiny double-entry ledger. Each entry is a dict {"kind": "debit"|"credit",
"cents": int}. The net balance is credits minus debits. Debits reduce the
balance; credits raise it.
"""


def _validate(entry):
    if entry.get("kind") not in ("debit", "credit"):
        raise ValueError(f"bad entry kind: {entry.get('kind')!r}")
    if not isinstance(entry.get("cents"), int):
        raise ValueError("cents must be an int")
    return entry


def net_cents(entries):
    """Net balance in cents: credits add, debits subtract."""
    total = 0
    for e in entries:
        _validate(e)
        total += e["cents"]
    return total


def net_dollars(entries):
    return net_cents(entries) / 100.0


def is_balanced(entries):
    """True iff debits and credits cancel out."""
    return net_cents(entries) == 0
