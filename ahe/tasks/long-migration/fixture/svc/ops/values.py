"""`values` — returns the legacy (ok, payload) tuple."""


def values(table):
    try:
        return (True, sorted(table.values()))
    except Exception:
        return (False, "not a mapping")


def values_or(default, table):
    ok, payload = values(table)
    if ok:
        return payload
    return default
