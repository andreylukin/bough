"""The published surface. PROTECTED: do not modify."""

from dataclasses import dataclass, field
from datetime import date

from .engine import Engine


@dataclass
class Rule:
    """A recurrence.

    freq:     "daily" | "weekly" | "monthly"
    start:    first candidate date
    interval: 1 = every period, 2 = every other, ...
    byday:    weekly only; ISO weekday numbers (Mon=1 .. Sun=7)
    count:    stop after this many occurrences
    until:    stop after this date (inclusive)
    exclude:  dates to drop from the result
    """

    freq: str
    start: date
    interval: int = 1
    byday: tuple = ()
    count: int | None = None
    until: date | None = None
    exclude: frozenset = field(default_factory=frozenset)


def expand(rule: Rule) -> list:
    """The occurrence dates, ascending."""
    return Engine(rule).run()
