"""The chain. Every stage takes the context dict and returns it, mutated."""

from typing import Dict


def authenticate(ctx: Dict) -> Dict:
    if ctx["path"].startswith("/public"):
        ctx["user"] = None
        return ctx
    ctx["user"] = "u-" + ctx["path"].strip("/").split("/")[0]
    return ctx


def trace(ctx: Dict) -> Dict:
    ctx["trace"] = "%s:%s" % (ctx["method"], ctx["path"])
    return ctx


def authorize(ctx: Dict) -> Dict:
    if ctx["method"] == "DELETE" and ctx["user"] is None:
        ctx["status"] = 403
        ctx["body"] = "forbidden"
    return ctx


def render(ctx: Dict) -> Dict:
    if ctx["status"] == 200:
        ctx["body"] = "%s %s by %s" % (ctx["method"], ctx["path"], ctx["user"])
    return ctx


CHAIN = [authenticate, trace, authorize, render]
