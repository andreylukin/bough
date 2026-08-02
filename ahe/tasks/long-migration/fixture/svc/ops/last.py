"""`last` — returns the legacy (ok, payload) tuple."""


def last(items):
    try:
        return (True, items[-1])
    except Exception:
        return (False, "empty sequence")


def last_or(default, items):
    ok, payload = last(items)
    if ok:
        return payload
    return default
