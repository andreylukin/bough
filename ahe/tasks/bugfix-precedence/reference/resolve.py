"""Resolution: given the layers, what is a key's value?

The reference fix. Three edits against the fixture, and a solution that makes only
one or two of them still fails:
  * `_pick` honours `pinned` from every source, not just file/env (spec R2)
  * `_merge_list` walks claims lowest-first, keeping first occurrences (spec R3)
  * `explain`'s own inlined merge had the same defect and is not covered by the
    checked-in suite — the one an agent finds only by reading, not by running
"""

from typing import Any, List

from .types import Layer, Value

# Lowest precedence first.
SOURCE_ORDER = ["defaults", "file", "env", "flags"]


def _source_order(layers: List[Layer]) -> List[Layer]:
    by_name = {layer.name: layer for layer in layers}
    return [by_name[name] for name in SOURCE_ORDER if name in by_name]


def _claims(layers: List[Layer], key: str) -> List[tuple]:
    """Every (layer, value) claiming `key`, lowest precedence first."""
    out = []
    for layer in _source_order(layers):
        value = layer.get(key)
        if value is not None:
            out.append((layer, value))
    return out


def _merge_list(claims: List[tuple]) -> List[Any]:
    """Concatenate every claim's items, dropping duplicates."""
    seen = set()
    out = []
    for _layer, value in claims:
        for item in value.raw:
            if item in seen:
                continue
            seen.add(item)
            out.append(item)
    return out


def _pick(claims: List[tuple]) -> Value:
    """The winning scalar claim."""
    pinned = [value for _layer, value in claims if value.pinned]
    if pinned:
        return pinned[-1]
    return claims[-1][1]


def resolve(layers: List[Layer], key: str) -> Any:
    claims = _claims(layers, key)
    if not claims:
        raise KeyError(key)
    if any(value.is_list for _layer, value in claims):
        return _merge_list(claims)
    return _pick(claims).raw


def explain(layers: List[Layer], key: str) -> str:
    """A human-readable account of the same decision `resolve` makes."""
    claims = _claims(layers, key)
    if not claims:
        raise KeyError(key)
    if any(value.is_list for _layer, value in claims):
        merged = _merge_list(claims)
        sources = ", ".join(layer.name for layer, _value in claims)
        return "%s = %r (merged from %s)" % (key, merged, sources)
    winner_value = _pick(claims)
    winner_layer = next(
        layer for layer, value in claims if value is winner_value
    )
    return "%s = %r (from %s)" % (key, winner_value.raw, winner_layer.name)
