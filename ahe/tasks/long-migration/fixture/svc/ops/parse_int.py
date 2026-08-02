"""`parse_int` — returns the legacy (ok, payload) tuple."""


def parse_int(text):
    try:
        return (True, int(text))
    except Exception:
        return (False, "not an integer")


def parse_int_or(default, text):
    ok, payload = parse_int(text)
    if ok:
        return payload
    return default
