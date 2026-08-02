"""`strip` — returns a Result."""

from svc.result import Err, Ok


def strip(text):
    try:
        return Ok(text.strip())
    except Exception:
        return Err("not text")


def strip_or(default, text):
    result = strip(text)
    if result.ok:
        return result.value
    return default
