"""Stage 1: parse raw sales lines into record dicts (fields stay strings)."""

FIELDS = ("date", "region", "product", "qty", "price")


def load_records(lines):
    """One dict per nonblank line; odd shapes are kept for validate to drop."""
    records = []
    for line in lines:
        line = line.strip()
        if not line:
            continue
        records.append(dict(zip(FIELDS, line.split())))
    return records
