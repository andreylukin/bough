"""`last` — returns a Result."""

from svc.result import Err, Ok


def last(items):
    try:
        return Ok(items[-1])
    except Exception:
        return Err("empty sequence")


def last_or(default, items):
    result = last(items)
    if result.ok:
        return result.value
    return default
