"""Module lib/session.py."""

from app.lib.conn import db, log


def op_0(ident):
    db.execute("SELECT * FROM sessions WHERE id = ?", (ident,))

def op_1(ident):
    db.execute("SELECT * FROM sessions WHERE id = %s" % ident)

