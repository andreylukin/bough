"""`upper` — returns a Result."""

from svc.result import Err, Ok


def upper(text):
    try:
        return Ok(text.upper())
    except Exception:
        return Err("not text")


def upper_or(default, text):
    result = upper(text)
    if result.ok:
        return result.value
    return default
