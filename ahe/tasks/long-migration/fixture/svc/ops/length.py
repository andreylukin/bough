"""`length` — returns the legacy (ok, payload) tuple."""


def length(items):
    try:
        return (True, len(items))
    except Exception:
        return (False, "no length")


def length_or(default, items):
    ok, payload = length(items)
    if ok:
        return payload
    return default
