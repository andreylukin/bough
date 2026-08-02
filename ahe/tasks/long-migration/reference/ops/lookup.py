"""`lookup` — returns a Result."""

from svc.result import Err, Ok


def lookup(table):
    try:
        return Ok(table['k'])
    except Exception:
        return Err("missing key")


def lookup_or(default, table):
    result = lookup(table)
    if result.ok:
        return result.value
    return default
