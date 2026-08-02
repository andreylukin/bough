"""`square` — returns a Result."""

from svc.result import Err, Ok


def square(n):
    try:
        return Ok(n * n)
    except Exception:
        return Err("not a number")


def square_or(default, n):
    result = square(n)
    if result.ok:
        return result.value
    return default
