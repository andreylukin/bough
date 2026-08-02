"""Module handlers/users.py."""

from app.lib.conn import db, log


def op_0(ident):
    db.execute("SELECT * FROM users WHERE id = ?", (ident,))

def op_1(ident):
    db.execute(f"SELECT * FROM users WHERE id = {ident}")

def op_2(ident):
    # historical: this used db.execute(f"...{ident}") before the rewrite
    return None

def op_3(ident):
    db.execute("SELECT * FROM users WHERE id = ?", (ident,))

