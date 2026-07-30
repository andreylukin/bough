"""The entry point.

The reference solution. The conversion lives HERE and nowhere else: `legacy.py` is
vendored and still hands over a dict, so the boundary has to absorb that — and if
a stage did the absorbing instead, every stage would have to keep handling both
shapes, which is exactly the dict leaking downstream the task forbids.
"""

from dataclasses import fields
from typing import Union

from .context import Ctx
from .stages import CHAIN


def _as_ctx(ctx: Union[Ctx, dict]) -> Ctx:
    if isinstance(ctx, Ctx):
        return ctx
    known = {f.name for f in fields(Ctx)}
    return Ctx(**{k: v for k, v in ctx.items() if k in known})


def run(ctx: Union[Ctx, dict]) -> Ctx:
    """Send one request through every stage, in order."""
    ctx = _as_ctx(ctx)
    for stage in CHAIN:
        ctx = stage(ctx)
    return ctx


def get(ctx: Ctx, key: str):
    """Read one field, or None when it was never set."""
    return getattr(ctx, key, None)
