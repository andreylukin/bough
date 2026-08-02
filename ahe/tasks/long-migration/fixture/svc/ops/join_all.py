"""`join_all` — returns the legacy (ok, payload) tuple."""


def join_all(items):
    try:
        return (True, ','.join(items))
    except Exception:
        return (False, "not all strings")


def join_all_or(default, items):
    ok, payload = join_all(items)
    if ok:
        return payload
    return default
