"""`first` — returns the legacy (ok, payload) tuple."""


def first(items):
    try:
        return (True, items[0])
    except Exception:
        return (False, "empty sequence")


def first_or(default, items):
    ok, payload = first(items)
    if ok:
        return payload
    return default
