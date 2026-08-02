"""Module handlers/search.py."""

from app.lib.conn import db, log


def op_0(ident):
    db.execute("SELECT * FROM docs WHERE id = %s" % ident)

def op_1(ident):
    # historical: this used db.execute(f"...{ident}") before the rewrite
    return None

def op_2(ident):
    db.execute("SELECT * FROM docs WHERE id = {}".format(ident))

