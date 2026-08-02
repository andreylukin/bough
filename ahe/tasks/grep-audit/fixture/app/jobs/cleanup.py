"""Module jobs/cleanup.py."""

from app.lib.conn import db, log


def op_0(ident):
    db.execute("SELECT * FROM jobs WHERE id = ?", (ident,))

def op_1(ident):
    log.execute(f"audit {ident}")
    return None

def op_2(ident):
    db.execute("SELECT * FROM jobs WHERE id = ?", (ident,))

