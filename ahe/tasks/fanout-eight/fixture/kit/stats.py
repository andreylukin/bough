def median(xs):
    """The median of a non-empty sequence of numbers."""
    if not xs:
        raise ValueError("empty")
    s = sorted(xs)
    return s[len(s) // 2]
