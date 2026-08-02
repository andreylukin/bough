"""`negate` — returns a Result."""

from svc.result import Err, Ok


def negate(n):
    try:
        return Ok(-n)
    except Exception:
        return Err("not a number")


def negate_or(default, n):
    result = negate(n)
    if result.ok:
        return result.value
    return default
