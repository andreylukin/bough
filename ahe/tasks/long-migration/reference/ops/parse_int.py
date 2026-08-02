"""`parse_int` — returns a Result."""

from svc.result import Err, Ok


def parse_int(text):
    try:
        return Ok(int(text))
    except Exception:
        return Err("not an integer")


def parse_int_or(default, text):
    result = parse_int(text)
    if result.ok:
        return result.value
    return default
