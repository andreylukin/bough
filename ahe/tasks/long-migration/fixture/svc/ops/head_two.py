"""`head_two` — returns the legacy (ok, payload) tuple."""


def head_two(items):
    try:
        return (True, list(items[:2]))
    except Exception:
        return (False, "not a sequence")


def head_two_or(default, items):
    ok, payload = head_two(items)
    if ok:
        return payload
    return default
