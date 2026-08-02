"""`invert` — returns the legacy (ok, payload) tuple."""


def invert(n):
    try:
        return (True, 1 / n)
    except Exception:
        return (False, "division by zero")


def invert_or(default, n):
    ok, payload = invert(n)
    if ok:
        return payload
    return default
