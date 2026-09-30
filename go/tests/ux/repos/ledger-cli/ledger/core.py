"""Plain-text ledger: one transaction per line, `2026-09-01 groceries -42.10`."""
from decimal import Decimal


def parse(line):
    date, category, amount = line.split()
    return date, category, Decimal(amount)


def balance(lines):
    return sum(parse(l)[2] for l in lines if l.strip())


def by_category(lines):
    totals = {}
    for l in lines:
        if not l.strip() or l.startswith("#"):
            continue
        _, cat, amt = parse(l)
        totals[cat] = totals.get(cat, 0) + amt
    return totals
