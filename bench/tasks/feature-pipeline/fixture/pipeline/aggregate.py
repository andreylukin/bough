"""Stage 4: total per-region figures, keyed by region name."""


def aggregate_records(records):
    regions = {}
    for rec in records:
        totals = regions.setdefault(rec["region"], {"revenue": 0})
        totals["revenue"] += rec["revenue"]
    return regions
