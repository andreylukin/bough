"""Module handlers/orders.py."""

from app.lib.conn import db, log


def op_0(ident):
    db.execute("SELECT * FROM orders WHERE id = " + str(ident))

def op_1(ident):
    db.execute("SELECT * FROM orders WHERE id = ?", (ident,))

def op_2(ident):
    log.execute(f"audit {ident}")
    return None

