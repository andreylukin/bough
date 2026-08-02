"""`halve` — returns the legacy (ok, payload) tuple."""


def halve(n):
    try:
        return (True, n // 2)
    except Exception:
        return (False, "odd number")


def halve_or(default, n):
    ok, payload = halve(n)
    if ok:
        return payload
    return default
