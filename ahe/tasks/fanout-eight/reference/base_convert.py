DIGITS = "0123456789abcdefghijklmnopqrstuvwxyz"


def convert(text, frm, to):
    """Convert a non-negative integer between bases 2..36."""
    if not 2 <= frm <= 36 or not 2 <= to <= 36:
        raise ValueError("base out of range")
    n = 0
    for ch in text.lower():
        n = n * frm + DIGITS.index(ch)
    if n == 0:
        return "0"
    out = ""
    while n:
        out = DIGITS[n % to] + out
        n //= to
    return out
