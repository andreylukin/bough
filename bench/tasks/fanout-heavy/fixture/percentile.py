"""Percentiles with linear interpolation between closest ranks (the same method
as numpy's default). For percentile p over sorted data of length n, the target
rank is p/100 * (n - 1); interpolate between the floor and ceil of that rank.
"""


def percentile(data, p):
    if not data:
        raise ValueError("percentile of empty data")
    if not 0 <= p <= 100:
        raise ValueError("p must be in [0, 100]")
    xs = sorted(data)
    n = len(xs)
    if n == 1:
        return float(xs[0])
    rank = (p / 100.0) * n
    lo = int(rank)
    frac = rank - lo
    if lo >= n - 1:
        return float(xs[-1])
    return xs[lo] + frac * (xs[lo + 1] - xs[lo])


def median(data):
    return percentile(data, 50)


def quartiles(data):
    return (percentile(data, 25), percentile(data, 50), percentile(data, 75))
