"""`head_two` — returns a Result."""

from svc.result import Err, Ok


def head_two(items):
    try:
        return Ok(list(items[:2]))
    except Exception:
        return Err("not a sequence")


def head_two_or(default, items):
    result = head_two(items)
    if result.ok:
        return result.value
    return default
