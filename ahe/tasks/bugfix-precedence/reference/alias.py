"""Key renaming.

The reference fix. The defect was that each layer was renamed with its OWN alias
map, so a rename declared in `flags` never reached a key sitting in `defaults` —
the two claims stayed under different names and never competed. The spec asks for
two phases: resolve one alias map across all sources by precedence, then apply that
single map to every source.
"""

from typing import Dict, List

from .types import Layer, Value

# Lowest precedence first. Duplicated from resolve.py deliberately: alias
# resolution is precedence-ordered too, and importing resolve.py here would be a
# cycle.
SOURCE_ORDER = ["defaults", "file", "env", "flags"]


def alias_map(layers: List[Layer]) -> Dict[str, str]:
    """The single alias map, with higher sources overriding lower ones."""
    by_name = {layer.name: layer for layer in layers}
    merged: Dict[str, str] = {}
    for name in SOURCE_ORDER:
        layer = by_name.get(name)
        if layer is not None:
            merged.update(layer.aliases)
    return merged


def canonical(name: str, aliases: Dict[str, str]) -> str:
    """The canonical form of one key under `aliases`. Renames do not chain."""
    return aliases.get(name, name)


def apply_aliases(layer: Layer, aliases: Dict[str, str]) -> Layer:
    """A copy of `layer` with its keys renamed under the resolved map."""
    renamed: Dict[str, Value] = {}
    for key, value in layer.values.items():
        renamed[canonical(key, aliases)] = value
    return Layer(layer.name, renamed, layer.aliases)


def normalize(layers: List[Layer]) -> List[Layer]:
    """Every layer with its keys in canonical form, under one shared map."""
    aliases = alias_map(layers)
    return [apply_aliases(layer, aliases) for layer in layers]
