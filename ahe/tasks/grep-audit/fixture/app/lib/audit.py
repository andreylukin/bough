"""Module lib/audit.py."""

from app.lib.conn import db, log


def op_0(ident):
    # historical: this used db.execute(f"...{ident}") before the rewrite
    return None

