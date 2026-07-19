"""Load ledger entries from raw 'name amount' lines."""

from normalize import parse_amount


def load_entries(lines):
    """Return a list of (name, cents) tuples, skipping blank lines."""
    entries = []
    for line in lines:
        line = line.strip()
        if not line:
            continue
        name, raw = line.rsplit(None, 1)
        entries.append((name, parse_amount(raw)))
    return entries
