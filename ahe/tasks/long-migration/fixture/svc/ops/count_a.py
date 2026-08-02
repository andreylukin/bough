"""`count_a` — returns the legacy (ok, payload) tuple."""


def count_a(text):
    try:
        return (True, text.count('a'))
    except Exception:
        return (False, "not text")


def count_a_or(default, text):
    ok, payload = count_a(text)
    if ok:
        return payload
    return default
