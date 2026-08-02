"""Module handlers/admin.py."""

from app.lib.conn import db, log


def op_0(ident):
    db.execute("SELECT * FROM admin WHERE id = ?", (ident,))

def op_1(ident):
    db.execute("SELECT * FROM admin WHERE id = ?", (ident,))

