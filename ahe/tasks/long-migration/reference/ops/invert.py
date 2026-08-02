"""`invert` — returns a Result."""

from svc.result import Err, Ok


def invert(n):
    try:
        return Ok(1 / n)
    except Exception:
        return Err("division by zero")


def invert_or(default, n):
    result = invert(n)
    if result.ok:
        return result.value
    return default
