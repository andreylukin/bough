"""`length` — returns a Result."""

from svc.result import Err, Ok


def length(items):
    try:
        return Ok(len(items))
    except Exception:
        return Err("no length")


def length_or(default, items):
    result = length(items)
    if result.ok:
        return result.value
    return default
