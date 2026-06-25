"""bough's mitmproxy addon — the programmable egress layer.

Replaces nono's declarative network policy with code you can extend. Reads a
per-session config (path in BOUGH_PROXY_CONFIG) and, on every flow:

  * default-deny allowlist — a request to a host not on `allow` is blocked (403),
    so egress is gated at L7 (method/path-aware if you extend it here);
  * credential injection — for hosts matching an `inject` rule, the real secret
    (read from this process's env, OUTSIDE the sandbox) is written into the auth
    header, so the sandboxed agent never holds it;
  * sniffing — every decision is logged for visibility.

Config shape (JSON):
  {
    "allow": ["api.github.com", "github.com"],
    "inject": [
      {"hosts": ["api.github.com"], "header": "Authorization",
       "format": "Bearer {}", "secret_env": "BOUGH_SECRET_github"},
      {"hosts": ["github.com"], "scheme": "basic",
       "user": "x-access-token", "secret_env": "BOUGH_SECRET_github"}
    ]
  }

`allow` empty/absent = allow all (sniff-only). Secrets live only in this
process's env; the agent's sandbox never sees them.
"""
import base64
import json
import logging
import os

from mitmproxy import http

log = logging.getLogger("bough")


_CACHE: dict = {"mtime": 0.0, "config": {}}


def _load() -> dict:
    """Read the session config, re-reading when the file changes so a session's
    allowlist can be updated without restarting the proxy."""
    path = os.environ.get("BOUGH_PROXY_CONFIG", "")
    if not path or not os.path.exists(path):
        return {}
    try:
        mtime = os.path.getmtime(path)
        if mtime != _CACHE["mtime"]:
            with open(path) as f:
                _CACHE["config"] = json.load(f)
            _CACHE["mtime"] = mtime
    except (OSError, ValueError) as e:
        log.warning("bough: bad proxy config %s: %s", path, e)
    return _CACHE["config"]


def _host_matches(host: str, patterns: list) -> bool:
    for p in patterns:
        if host == p or (p.startswith("*.") and host.endswith(p[1:])):
            return True
    return False


def request(flow: http.HTTPFlow) -> None:
    config = _load()
    host = flow.request.pretty_host

    allow = config.get("allow") or []
    if allow and not _host_matches(host, allow):
        log.info("bough: BLOCK %s (not in allowlist)", host)
        flow.response = http.Response.make(
            403, b"blocked by bough egress policy\n",
            {"Content-Type": "text/plain"},
        )
        return

    for rule in config.get("inject", []):
        if not _host_matches(host, rule.get("hosts", [])):
            continue
        secret = os.environ.get(rule.get("secret_env", ""), "")
        if not secret:
            continue
        if rule.get("scheme") == "basic":
            user = rule.get("user", "x-access-token")
            token = base64.b64encode(f"{user}:{secret}".encode()).decode()
            flow.request.headers["Authorization"] = "Basic " + token
        else:
            header = rule.get("header", "Authorization")
            fmt = rule.get("format", "Bearer {}")
            flow.request.headers[header] = fmt.replace("{}", secret)
        log.info("bough: inject creds for %s", host)
