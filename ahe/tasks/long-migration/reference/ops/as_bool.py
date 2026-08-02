"""`as_bool` — returns a Result."""

from svc.result import Err, Ok


def as_bool(n):
    try:
        return Ok(bool(n))
    except Exception:
        return Err("not truthy-able")


def as_bool_or(default, n):
    result = as_bool(n)
    if result.ok:
        return result.value
    return default
