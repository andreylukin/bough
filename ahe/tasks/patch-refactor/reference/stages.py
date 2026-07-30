"""The chain. Every stage takes a Ctx and returns a new one.

The reference solution. Each stage that changes something returns
`dataclasses.replace(ctx, ...)` rather than assigning — which is what makes the
order they run in visible in the data flow instead of hidden in shared state.
"""

from dataclasses import replace

from .context import Ctx


def authenticate(ctx: Ctx) -> Ctx:
    if ctx.path.startswith("/public"):
        return replace(ctx, user=None)
    return replace(ctx, user="u-" + ctx.path.strip("/").split("/")[0])


def trace(ctx: Ctx) -> Ctx:
    return replace(ctx, trace="%s:%s" % (ctx.method, ctx.path))


def authorize(ctx: Ctx) -> Ctx:
    if ctx.method == "DELETE" and ctx.user is None:
        return replace(ctx, status=403, body="forbidden")
    return ctx


def render(ctx: Ctx) -> Ctx:
    if ctx.status == 200:
        return replace(ctx, body="%s %s by %s" % (ctx.method, ctx.path, ctx.user))
    return ctx


CHAIN = [authenticate, trace, authorize, render]
