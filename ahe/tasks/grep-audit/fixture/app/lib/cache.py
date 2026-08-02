"""Module lib/cache.py."""

from app.lib.conn import db, log


def op_0(ident):
    log.execute(f"audit {ident}")
    return None

def op_1(ident):
    db.execute("SELECT * FROM cache WHERE id = ?", (ident,))

