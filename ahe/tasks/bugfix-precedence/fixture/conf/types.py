"""The value and layer types. Stable; the resolver is what interprets them."""

from dataclasses import dataclass, field
from typing import Any, Dict


@dataclass(frozen=True)
class Value:
    """One source's claim about one key. `pinned` asserts it over higher sources."""

    raw: Any
    pinned: bool = False

    @property
    def is_list(self) -> bool:
        return isinstance(self.raw, list)


@dataclass
class Layer:
    """One configuration source."""

    name: str
    values: Dict[str, Value] = field(default_factory=dict)

    def get(self, key: str):
        return self.values.get(key)
