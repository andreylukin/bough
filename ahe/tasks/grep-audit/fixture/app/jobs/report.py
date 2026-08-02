"""Module jobs/report.py."""

from app.lib.conn import db, log


def op_0(ident):
    db.execute(f"SELECT * FROM metrics WHERE id = {ident}")

