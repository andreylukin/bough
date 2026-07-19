"""Normalization helpers for raw ledger text."""


def parse_amount(text):
    """Parse a dollar amount like '$1,234.56' into integer cents."""
    cleaned = text.strip().replace("$", "").replace(",", "")
    return int(float(cleaned) * 100)
