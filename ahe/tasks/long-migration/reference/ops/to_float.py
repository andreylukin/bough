"""`to_float` — returns a Result."""

from svc.result import Err, Ok


def to_float(text):
    try:
        return Ok(float(text))
    except Exception:
        return Err("not a float")


def to_float_or(default, text):
    result = to_float(text)
    if result.ok:
        return result.value
    return default
