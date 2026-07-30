"""Key renaming.

Sources rename keys as they evolve: `bind_port` became `port`, `tag` became `tags`.
A layer written against the old name has to end up contributing to the new one.
"""

from typing import Dict, List

from .types import Layer, Value


def alias_map(layer: Layer) -> Dict[str, str]:
    """The renames this layer declares."""
    return dict(layer.aliases)


def canonical(name: str, aliases: Dict[str, str]) -> str:
    """The canonical form of one key under `aliases`. Renames do not chain."""
    return aliases.get(name, name)


def apply_aliases(layer: Layer) -> Layer:
    """A copy of `layer` with its keys renamed."""
    aliases = alias_map(layer)
    renamed: Dict[str, Value] = {}
    for key, value in layer.values.items():
        renamed[canonical(key, aliases)] = value
    return Layer(layer.name, renamed, layer.aliases)


def normalize(layers: List[Layer]) -> List[Layer]:
    """Every layer with its keys in canonical form."""
    return [apply_aliases(layer) for layer in layers]
