"""`count_a` — returns a Result."""

from svc.result import Err, Ok


def count_a(text):
    try:
        return Ok(text.count('a'))
    except Exception:
        return Err("not text")


def count_a_or(default, text):
    result = count_a(text)
    if result.ok:
        return result.value
    return default
