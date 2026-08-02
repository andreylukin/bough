"""The published surface. PROTECTED: do not modify."""

from dataclasses import dataclass

from .scan import Scanner


@dataclass(frozen=True)
class Token:
    kind: str      # "word" | "number" | "punct"
    text: str
    start: int     # inclusive, in CODE POINTS
    end: int       # exclusive, in code points
    bstart: int    # inclusive, in UTF-8 BYTES
    bend: int      # exclusive, in bytes


def tokenize(text: str) -> list:
    """Split `text` into tokens, each carrying both offset systems."""
    return Scanner(text).run()
