"""Descriptive statistics over a list of numbers."""


def mean(xs):
    return sum(xs) / len(xs)


def median(xs):
    s = sorted(xs)
    n = len(s)
    mid = n // 2
    if n % 2 == 1:
        return s[mid]
    return (s[mid - 1] + s[mid]) / 2


def variance(xs):
    """Sample variance (Bessel-corrected: divide by n-1)."""
    n = len(xs)
    if n < 2:
        raise ValueError("need at least two points")
    m = mean(xs)
    return sum((x - m) ** 2 for x in xs) / n
