"""`keys` — returns a Result."""

from svc.result import Err, Ok


def keys(table):
    try:
        return Ok(sorted(table))
    except Exception:
        return Err("not a mapping")


def keys_or(default, table):
    result = keys(table)
    if result.ok:
        return result.value
    return default
