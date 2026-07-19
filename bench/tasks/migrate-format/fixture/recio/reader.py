"""Read record files in the v1 format: one record per line,
``id,name,qty,note`` -- note is everything after the third comma."""


def read_records(path):
    """Return records as dicts of strings with keys id, name, qty, note."""
    records = []
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            if not line.strip():
                continue
            rec_id, name, qty, note = line.split(",", 3)
            records.append({"id": rec_id, "name": name, "qty": qty, "note": note})
    return records
