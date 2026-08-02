"""`sum_all` — returns a Result."""

from svc.result import Err, Ok


def sum_all(items):
    try:
        return Ok(sum(items))
    except Exception:
        return Err("not summable")


def sum_all_or(default, items):
    result = sum_all(items)
    if result.ok:
        return result.value
    return default
