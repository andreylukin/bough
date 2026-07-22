"""Convert non-negative integers to and from an arbitrary base (2..36)."""

_DIGITS = "0123456789abcdefghijklmnopqrstuvwxyz"


def to_base(n, base):
    if not 2 <= base <= 36:
        raise ValueError("base out of range")
    if n == 0:
        return "0"
    out = []
    while n > 0:
        n, rem = divmod(n, base)
        out.append(_DIGITS[rem])
    return "".join(out)


def from_base(s, base):
    n = 0
    for ch in s:
        n = n * base + _DIGITS.index(ch)
    return n
