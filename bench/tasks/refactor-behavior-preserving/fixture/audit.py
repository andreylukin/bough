"""Audit trail: count records per location."""


def audit(lines):
    by_loc = {}
    for line in lines:
        line = line.split("#", 1)[0]
        if not line.strip():
            continue
        parts = line.split("|")
        if len(parts) != 3:
            raise ValueError(f"bad record: {line!r}")
        sku, qty, loc = parts
        qty = int(qty)
        by_loc[loc] = by_loc.get(loc, 0) + 1
    return by_loc
