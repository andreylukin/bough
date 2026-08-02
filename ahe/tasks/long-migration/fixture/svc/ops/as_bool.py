"""`as_bool` — returns the legacy (ok, payload) tuple."""


def as_bool(n):
    try:
        return (True, bool(n))
    except Exception:
        return (False, "not truthy-able")


def as_bool_or(default, n):
    ok, payload = as_bool(n)
    if ok:
        return payload
    return default
