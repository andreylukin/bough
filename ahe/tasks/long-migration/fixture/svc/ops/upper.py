"""`upper` — returns the legacy (ok, payload) tuple."""


def upper(text):
    try:
        return (True, text.upper())
    except Exception:
        return (False, "not text")


def upper_or(default, text):
    ok, payload = upper(text)
    if ok:
        return payload
    return default
