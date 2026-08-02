"""The published surface. PROTECTED: do not modify."""

from dataclasses import dataclass

from .rewrite import Rewriter


@dataclass(frozen=True)
class Scan:
    """A base table. `columns` is what it provides."""
    table: str
    columns: frozenset


@dataclass(frozen=True)
class Filter:
    """`pred` is a tuple: ("and", a, b) | ("or", a, b) | ("cmp", column, op, value)."""
    child: object
    pred: tuple


@dataclass(frozen=True)
class Join:
    left: object
    right: object
    on: tuple      # (left_column, right_column)
    kind: str = "inner"   # "inner" | "left"


def pushdown(node):
    """Push filters as close to the scans as they can legally go."""
    return Rewriter().run(node)
