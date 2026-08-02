"""`values` — returns a Result."""

from svc.result import Err, Ok


def values(table):
    try:
        return Ok(sorted(table.values()))
    except Exception:
        return Err("not a mapping")


def values_or(default, table):
    result = values(table)
    if result.ok:
        return result.value
    return default
