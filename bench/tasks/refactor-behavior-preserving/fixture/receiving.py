"""Receiving dock: total received quantity per SKU."""


def receive(lines):
    totals = {}
    for line in lines:
        if not line.strip():
            continue
        parts = line.split("|")
        if len(parts) != 3:
            raise ValueError(f"bad record: {line!r}")
        sku, qty, loc = (p.strip() for p in parts)
        qty = int(qty)
        totals[sku] = totals.get(sku, 0) + qty
    return totals
