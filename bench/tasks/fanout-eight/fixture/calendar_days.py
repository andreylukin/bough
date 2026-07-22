"""Date arithmetic on the proleptic Gregorian calendar, stdlib-free."""

_DAYS = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]


def is_leap(y):
    return y % 4 == 0 and (y % 100 != 0 and y % 400 == 0)


def days_in_month(y, m):
    if m == 2 and is_leap(y):
        return 29
    return _DAYS[m - 1]


def _ordinal(y, m, d):
    """Days since 0001-01-01 (that date is ordinal 1)."""
    n = d
    for mm in range(1, m):
        n += days_in_month(y, mm)
    for yy in range(1, y):
        n += 366 if is_leap(yy) else 365
    return n


def days_between(a, b):
    """a, b are (year, month, day) tuples; returns b - a in days."""
    return _ordinal(*b) - _ordinal(*a)
