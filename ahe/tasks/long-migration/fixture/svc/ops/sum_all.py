"""`sum_all` — returns the legacy (ok, payload) tuple."""


def sum_all(items):
    try:
        return (True, sum(items))
    except Exception:
        return (False, "not summable")


def sum_all_or(default, items):
    ok, payload = sum_all(items)
    if ok:
        return payload
    return default
