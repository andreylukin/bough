"""bough's mitmproxy addon — the programmable egress layer.

A programmable egress policy you can extend in code. Reads a
per-session config (path in BOUGH_PROXY_CONFIG) and gates every connection:

  * default-deny allowlist — a CONNECT (or plain request) to a host not on
    `allow` is blocked (403). Enforced at the CONNECT/host layer so it covers
    passthrough hosts too, and again at L7 for hosts we decrypt;
  * passthrough — hosts on `passthrough` are tunnelled WITHOUT interception, so
    a client that won't trust our CA (e.g. `gh`, a Go binary that ignores
    SSL_CERT_FILE on macOS) talks straight to the real server with its real
    cert. Still host-gated by the allowlist; we just can't see inside the TLS,
    so credentials for these hosts must travel in the sandbox env, not injected;
  * credential injection — for decrypted hosts matching an `inject` rule, the
    real secret (read from this process's env, OUTSIDE the sandbox) is written
    into the auth header, so the sandboxed agent never holds it;
  * sniffing — every decision is logged for visibility.

Config shape (JSON):
  {
    "allow": ["api.github.com", "github.com"],
    "passthrough": ["api.github.com", "github.com"],
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

from mitmproxy import http, tls

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


def _blocked(host: str, config: dict) -> bool:
    """True if `host` is not permitted by the allowlist (empty = allow all)."""
    allow = config.get("allow") or []
    return bool(allow) and not _host_matches(host, allow)


def http_connect(flow: http.HTTPFlow) -> None:
    """Gate the CONNECT before any bytes flow. This is the only gate passthrough
    hosts get (we never decrypt them), so the allowlist is enforced here at the
    host level for every HTTPS connection, intercepted or not."""
    config = _load()
    host = flow.request.host
    if _blocked(host, config):
        log.info("bough: BLOCK CONNECT %s (not in allowlist)", host)
        flow.response = http.Response.make(
            403, b"blocked by bough egress policy\n",
            {"Content-Type": "text/plain"},
        )
    else:
        log.info("bough: ALLOW CONNECT %s", host)


def tls_clienthello(data: tls.ClientHelloData) -> None:
    """Tunnel `passthrough` hosts as-is (no interception), so a client that won't
    trust our CA reaches the real server. Host-gating already happened in
    http_connect; we just decline to decrypt."""
    config = _load()
    passthrough = config.get("passthrough") or []
    sni = data.client_hello.sni or ""
    if sni and _host_matches(sni, passthrough):
        log.info("bough: PASSTHROUGH %s (not intercepted)", sni)
        data.ignore_connection = True


def request(flow: http.HTTPFlow) -> None:
    config = _load()
    host = flow.request.pretty_host

    if _blocked(host, config):
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
