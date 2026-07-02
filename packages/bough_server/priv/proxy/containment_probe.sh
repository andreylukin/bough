#!/usr/bin/env bash
# Layer 1 — containment probes for bough's agent container.
#
# Runs adversarial network probes INSIDE a running container and asserts each is
# BLOCKED. This is the load-bearing test: if any probe SUCCEEDS, the sandbox has
# a route out that bypasses the proxy gate -> overall FAIL (non-zero exit).
#
# Usage: containment_probe.sh <container> [proxy_host:port]
#   <container>   name/id of the running agent container (docker exec target)
#   proxy_host:port  optional positive control: the ONE endpoint that SHOULD be
#                    reachable (the proxy sidecar), to prove the container isn't
#                    simply offline (which would pass every probe trivially).
set -uo pipefail

CONTAINER="${1:?usage: containment_probe.sh <container> [proxy_host:port]}"
PROXY="${2:-}"

pass=0
fail=0
dex() { docker exec "$CONTAINER" "$@"; }

# assert_blocked <name> <cmd...>: PASS if the command fails, FAIL if it succeeds.
assert_blocked() {
  local name="$1"; shift
  if dex "$@" >/dev/null 2>&1; then
    echo "FAIL  $name — reachable (boundary leak!)"
    fail=$((fail + 1))
  else
    echo "pass  $name — blocked"
    pass=$((pass + 1))
  fi
}

echo "== containment probes: $CONTAINER =="

# Preflight: missing tools would make probes false-pass. Warn loudly.
dex sh -c 'command -v curl >/dev/null'    || echo "WARN  curl missing — curl probes may FALSE-PASS"
dex sh -c 'command -v python3 >/dev/null' || echo "WARN  python3 missing — socket/dns probes may FALSE-PASS"

# 1. Direct HTTPS to a public IP, explicitly bypassing any proxy env var.
assert_blocked "direct-https (bypass proxy)" \
  sh -c 'curl -s --noproxy "*" --max-time 5 -o /dev/null https://1.1.1.1'

# 2. Raw TCP socket to an external host:443 — proves enforcement is BELOW the
#    app layer (a client that ignores proxy env still can't get out).
assert_blocked "raw-tcp-443" \
  python3 -c 'import socket; socket.create_connection(("1.1.1.1", 443), 5)'

# 3. Direct UDP DNS query to a public resolver — the DNS-tunnel exfil channel
#    (the hole Anthropic's devcontainer firewall shipped with). A valid query
#    for example.com; a reply means the channel is open.
assert_blocked "direct-dns-udp53" \
  python3 -c 'import socket; s=socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(5); s.sendto(b"\xaa\xaa\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01", ("8.8.8.8", 53)); s.recvfrom(512)'

# 4. Cloud instance-metadata endpoint (SSRF / credential-theft target).
assert_blocked "cloud-metadata-169.254.169.254" \
  sh -c 'curl -s --max-time 5 -o /dev/null http://169.254.169.254/latest/meta-data/'

# 5. With every proxy env var unset, egress must STILL be caught (transparent
#    redirect) or refused — never silently allowed.
assert_blocked "no-proxy-env-egress" \
  sh -c 'unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; curl -s --max-time 5 -o /dev/null https://example.com'

# Positive control: the proxy endpoint is the one thing that SHOULD be reachable.
if [ -n "$PROXY" ]; then
  if dex sh -c "curl -s --max-time 5 -o /dev/null http://$PROXY"; then
    echo "pass  proxy-reachable ($PROXY) — sanity ok"
    pass=$((pass + 1))
  else
    echo "WARN  proxy-reachable ($PROXY) — proxy NOT reachable; is the sidecar up?"
  fi
fi

echo "== $pass passed, $fail failed =="
[ "$fail" -eq 0 ]
