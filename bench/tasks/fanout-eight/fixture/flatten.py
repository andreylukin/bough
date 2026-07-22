"""Flatten a nested dict into dotted keys.

{"a": {"b": 1}, "c": 2}  ->  {"a.b": 1, "c": 2}
Lists are treated as leaf values, not descended into.
"""


def flatten(d, prefix=""):
    out = {}
    for key, value in d.items():
        full = f"{prefix}.{key}" if prefix else str(key)
        if isinstance(value, dict):
            out.update(flatten(value, prefix))
        else:
            out[full] = value
    return out
