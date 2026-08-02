"""`lookup` — returns the legacy (ok, payload) tuple."""


def lookup(table):
    try:
        return (True, table['k'])
    except Exception:
        return (False, "missing key")


def lookup_or(default, table):
    ok, payload = lookup(table)
    if ok:
        return payload
    return default
