"""`square` — returns the legacy (ok, payload) tuple."""


def square(n):
    try:
        return (True, n * n)
    except Exception:
        return (False, "not a number")


def square_or(default, n):
    ok, payload = square(n)
    if ok:
        return payload
    return default
