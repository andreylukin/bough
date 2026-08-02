"""`first` — returns a Result."""

from svc.result import Err, Ok


def first(items):
    try:
        return Ok(items[0])
    except Exception:
        return Err("empty sequence")


def first_or(default, items):
    result = first(items)
    if result.ok:
        return result.value
    return default
