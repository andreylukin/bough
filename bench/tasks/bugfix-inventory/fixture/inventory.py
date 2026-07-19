"""Tiny inventory ledger: apply (item, delta) movements to stock counts."""


def apply_movements(stock, movements):
    """Return a new dict with movements applied; stock never goes negative."""
    out = dict(stock)
    for item, delta in movements:
        new = out.get(item, 0) + delta
        if new < 0:
            raise ValueError(f"stock for {item} would go negative")
        out[item] = new
    return out


def low_stock(stock, threshold):
    """Names of items strictly below threshold, sorted."""
    return sorted(i for i, n in stock.items() if n <= threshold)
