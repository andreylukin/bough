"""Stage 3: convert types and derive revenue in integer cents."""


def transform_records(records):
    out = []
    for rec in records:
        out.append(
            {
                "date": rec["date"],
                "region": rec["region"],
                "product": rec["product"],
                "qty": int(rec["qty"]),
                "revenue": int(rec["qty"]) * int(rec["price"]),
            }
        )
    return out
