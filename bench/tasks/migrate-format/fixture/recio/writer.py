"""Write record files in the v1 comma-separated format."""


def write_records(path, records):
    with open(path, "w") as f:
        for rec in records:
            f.write("{},{},{},{}\n".format(rec["id"], rec["name"], rec["qty"], rec["note"]))
