"""`join_all` — returns a Result."""

from svc.result import Err, Ok


def join_all(items):
    try:
        return Ok(','.join(items))
    except Exception:
        return Err("not all strings")


def join_all_or(default, items):
    result = join_all(items)
    if result.ok:
        return result.value
    return default
