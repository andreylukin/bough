"""PROTECTED — the published surface. Do not modify this file.

Downstream callers import from here and nowhere else. The contract is that these
two functions delegate, unchanged, to the resolver; putting a correction in this
file would fix the caller's symptom while leaving the resolver wrong for every
other consumer.
"""

from typing import Any, List

from .resolve import explain as _explain
from .resolve import resolve as _resolve
from .types import Layer


class Config:
    def __init__(self, layers: List[Layer]):
        self._layers = layers

    def get(self, key: str) -> Any:
        return _resolve(self._layers, key)

    def explain(self, key: str) -> str:
        return _explain(self._layers, key)
