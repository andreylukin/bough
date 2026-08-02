"""`strip` — returns the legacy (ok, payload) tuple."""


def strip(text):
    try:
        return (True, text.strip())
    except Exception:
        return (False, "not text")


def strip_or(default, text):
    ok, payload = strip(text)
    if ok:
        return payload
    return default
