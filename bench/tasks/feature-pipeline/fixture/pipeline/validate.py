"""Stage 2: drop records that fail basic shape and type checks."""

from pipeline.load import FIELDS


def validate_records(records):
    valid = []
    for rec in records:
        if len(rec) != len(FIELDS):
            continue
        if not rec["qty"].isdigit() or not rec["price"].isdigit():
            continue
        valid.append(rec)
    return valid
