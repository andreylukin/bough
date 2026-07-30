"""The entry point."""

from typing import Dict

from .stages import CHAIN


def run(ctx: Dict) -> Dict:
    """Send one request through every stage, in order."""
    for stage in CHAIN:
        ctx = stage(ctx)
    return ctx


def get(ctx: Dict, key: str):
    """Read one field, or None when it was never set."""
    return ctx.get(key)
