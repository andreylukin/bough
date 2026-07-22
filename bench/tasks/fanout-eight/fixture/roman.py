"""Roman numeral conversion (values 1..3999)."""

_NUMERALS = [
    (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"),
    (100, "C"), (90, "XC"), (50, "L"), (40, "XL"),
    (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
]

_VALUES = {"I": 1, "V": 5, "X": 10, "L": 50, "C": 100, "D": 500, "M": 1000}


def to_roman(n):
    if not 1 <= n <= 3999:
        raise ValueError("out of range")
    out = []
    for value, sym in _NUMERALS:
        count, n = divmod(n, value)
        out.append(sym * count)
    return "".join(out)


def from_roman(s):
    total = 0
    prev = 0
    for ch in reversed(s):
        v = _VALUES[ch]
        if v < prev:
            total -= v
        else:
            total += v
    return total
