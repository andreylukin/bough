"""Request classifier + egress policy decision for bough's gate.

Pure, dependency-free logic so it can be unit-tested without mitmproxy or a
network. The mitmproxy addon (bough_proxy.py) adapts a real flow into a
`Request` and calls `decide`.

Scope: we only READ requests to gate them (allow/deny) — we never modify them,
so AWS SigV4 signatures stay valid. Credential injection is a separate concern
(see bough_proxy.py). The policy is default-deny at the host layer (matching the
addon today) and fail-closed on unrecognised actions.

What "action" means per provider:
  * AWS   — JSON-protocol services carry `X-Amz-Target: Service_Ver.Operation`;
            query-protocol services (ec2/sts/iam) carry `Action=Foo` in the body
            or query string. Read ops start with describe/list/get/... .
  * k8s   — the HTTP verb is the action (GET=read, DELETE/POST/PUT/PATCH=write);
            the resource is the path. The cluster host isn't fixed, so callers
            pass the set of API-server hosts.
  * GitHub— REST is verb+path; GraphQL is one `POST /graphql` with the operation
            in the body, so we peek for a `mutation` (coarse — see the note).
"""
from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from urllib.parse import parse_qs

READ = "read"
WRITE = "write"
UNKNOWN = "unknown"

# AWS operation name prefixes that are read-only (case-insensitive).
_AWS_READ_PREFIXES = (
    "describe", "list", "get", "head", "lookup", "search", "query", "scan",
    "batchget", "select", "estimate", "preview", "validate", "check", "view",
)


@dataclass
class Request:
    """A provider-agnostic view of one outbound request, built by the addon."""

    host: str
    method: str
    path: str
    headers: dict = field(default_factory=dict)
    body: bytes = b""


@dataclass
class Action:
    service: str  # "aws:ec2", "k8s", "github", "other"
    verb: str  # e.g. "TerminateInstances", "DELETE /api/v1/pods/x", "graphql:mutation"
    kind: str  # READ | WRITE | UNKNOWN


@dataclass
class Decision:
    allow: bool
    reason: str
    action: Action


@dataclass
class Policy:
    """Egress policy. `allow_hosts` gates at the host layer (empty = allow all
    hosts, sniff-only). `mode` gates at the action layer: "read_only" permits
    reads and blocks writes; "all" permits any action on an allowed host.
    `allow_verbs`/`deny_verbs` are explicit per-action overrides."""

    allow_hosts: set = field(default_factory=set)
    k8s_hosts: set = field(default_factory=set)
    mode: str = "read_only"
    allow_verbs: set = field(default_factory=set)
    deny_verbs: set = field(default_factory=set)


def _header(req: Request, name: str) -> str | None:
    lower = name.lower()
    for k, v in req.headers.items():
        if k.lower() == lower:
            return v
    return None


def _body_text(req: Request) -> str:
    if isinstance(req.body, bytes):
        return req.body.decode("utf-8", "replace")
    return req.body or ""


def _host_matches(host: str, patterns) -> bool:
    for p in patterns:
        if host == p or (p.startswith("*.") and host.endswith(p[1:])):
            return True
    return False


# ---- per-provider classifiers -------------------------------------------------


def _aws_kind(op: str) -> str:
    return READ if op.lower().startswith(_AWS_READ_PREFIXES) else WRITE


def classify_aws(req: Request) -> Action:
    service = "aws:" + req.host.split(".")[0]
    target = _header(req, "X-Amz-Target")
    if target:
        op = target.split(".")[-1]
        return Action(service, op, _aws_kind(op))
    # query protocol: Action= in the body, else in the path's query string
    for source in (_body_text(req), req.path.split("?", 1)[1] if "?" in req.path else ""):
        action = parse_qs(source).get("Action")
        if action:
            return Action(service, action[0], _aws_kind(action[0]))
    return Action(service, "?", UNKNOWN)


def classify_k8s(req: Request) -> Action:
    verb = req.method.upper()
    kind = READ if verb in ("GET", "HEAD") else WRITE if verb in ("POST", "PUT", "PATCH", "DELETE") else UNKNOWN
    resource = req.path.split("?", 1)[0]
    return Action("k8s", f"{verb} {resource}", kind)


def classify_github(req: Request) -> Action:
    path = req.path.split("?", 1)[0]
    if path.rstrip("/").endswith("/graphql"):
        text = _body_text(req)
        try:
            obj = json.loads(text)
            text = obj.get("query", text) if isinstance(obj, dict) else text
        except ValueError:
            pass
        # Coarse: a top-level `mutation` keyword means a write. Real gating would
        # tokenise the query; this is enough to split reads from writes.
        is_write = bool(re.search(r"\bmutation\b", text))
        return Action("github", "graphql:mutation" if is_write else "graphql:query", WRITE if is_write else READ)
    verb = req.method.upper()
    kind = READ if verb in ("GET", "HEAD") else WRITE
    return Action("github", f"{verb} {path}", kind)


def classify(req: Request, k8s_hosts=()) -> Action:
    host = req.host.lower()
    if _host_matches(host, k8s_hosts):
        return classify_k8s(req)
    if host == "amazonaws.com" or host.endswith(".amazonaws.com"):
        return classify_aws(req)
    if host == "github.com" or host.endswith(".github.com"):
        return classify_github(req)
    return Action("other", f"{req.method.upper()} {req.path.split('?', 1)[0]}", UNKNOWN)


# ---- decision ----------------------------------------------------------------


def decide(req: Request, policy: Policy) -> Decision:
    host = req.host.lower()
    if policy.allow_hosts and not _host_matches(host, policy.allow_hosts):
        return Decision(False, f"host {host} not in allowlist", Action("?", "?", UNKNOWN))

    action = classify(req, policy.k8s_hosts)

    if action.verb in policy.deny_verbs:
        return Decision(False, f"{action.verb} explicitly denied", action)
    if action.verb in policy.allow_verbs:
        return Decision(True, f"{action.verb} explicitly allowed", action)

    if policy.mode == "all":
        return Decision(True, "host allowed; mode=all", action)

    # mode == "read_only": reads pass, writes blocked, unknown fails closed.
    if action.kind == READ:
        return Decision(True, f"read action {action.verb}", action)
    if action.kind == WRITE:
        return Decision(False, f"write action {action.verb} blocked (mode=read_only)", action)
    return Decision(False, f"unknown action {action.verb} blocked (fail closed)", action)
