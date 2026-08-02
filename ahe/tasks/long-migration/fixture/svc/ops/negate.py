"""`negate` — returns the legacy (ok, payload) tuple."""


def negate(n):
    try:
        return (True, -n)
    except Exception:
        return (False, "not a number")


def negate_or(default, n):
    ok, payload = negate(n)
    if ok:
        return payload
    return default
