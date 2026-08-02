"""The published surface. PROTECTED: do not modify."""

from .split import Splitter


def allocate(total: int, weights: list) -> list:
    """Split `total` cents across `weights`. Returns one integer per weight."""
    return Splitter(weights).allocate(total)


def apply_rate(amount: int, rate_num: int, rate_den: int) -> int:
    """`amount` * rate_num / rate_den, rounded to whole cents."""
    return Splitter([]).apply_rate(amount, rate_num, rate_den)
