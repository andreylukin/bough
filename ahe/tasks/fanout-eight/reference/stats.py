def median(xs):
    """The median of a non-empty sequence of numbers."""
    if not xs:
        raise ValueError("empty")
    s = sorted(xs)
    mid = len(s) // 2
    if len(s) % 2:
        return s[mid]
    return (s[mid - 1] + s[mid]) / 2
