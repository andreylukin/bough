"""`repeat` — returns a Result."""

from svc.result import Err, Ok


def repeat(text):
    try:
        return Ok(text * 2)
    except Exception:
        return Err("not repeatable")


def repeat_or(default, text):
    result = repeat(text)
    if result.ok:
        return result.value
    return default
