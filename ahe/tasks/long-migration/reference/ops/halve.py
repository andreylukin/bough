"""`halve` — returns a Result."""

from svc.result import Err, Ok


def halve(n):
    try:
        return Ok(n // 2)
    except Exception:
        return Err("odd number")


def halve_or(default, n):
    result = halve(n)
    if result.ok:
        return result.value
    return default
