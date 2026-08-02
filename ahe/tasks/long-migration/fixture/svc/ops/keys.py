"""`keys` — returns the legacy (ok, payload) tuple."""


def keys(table):
    try:
        return (True, sorted(table))
    except Exception:
        return (False, "not a mapping")


def keys_or(default, table):
    ok, payload = keys(table)
    if ok:
        return payload
    return default
