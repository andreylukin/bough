"""`to_float` — returns the legacy (ok, payload) tuple."""


def to_float(text):
    try:
        return (True, float(text))
    except Exception:
        return (False, "not a float")


def to_float_or(default, text):
    ok, payload = to_float(text)
    if ok:
        return payload
    return default
