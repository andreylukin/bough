"""`repeat` — returns the legacy (ok, payload) tuple."""


def repeat(text):
    try:
        return (True, text * 2)
    except Exception:
        return (False, "not repeatable")


def repeat_or(default, text):
    ok, payload = repeat(text)
    if ok:
        return payload
    return default
