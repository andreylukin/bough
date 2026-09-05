#!/usr/bin/env bash
# Step 1 of 2: create the droplet with a temporary SSH hole open to
# this machine's IP only. Step 2 (deploy/lockdown.sh) closes it once
# Tailscale is up. No Tailscale auth key is involved: the droplet's
# login is approved interactively in a browser.
#
#   ./deploy/provision.sh [name]
set -euo pipefail

NAME="${1:-bough}"
REGION="${REGION:-nyc1}"
SIZE="${SIZE:-s-2vcpu-4gb}"
IMAGE="${IMAGE:-ubuntu-24-04-x64}"

cd "$(dirname "$0")"
doctl account get >/dev/null

MYIP=$(curl -fsS https://api.ipify.org)
echo "opening SSH to $MYIP only"

KEYS=$(doctl compute ssh-key list --no-header --format ID | paste -sd, -)
[ -n "$KEYS" ] || { echo "no SSH keys on the DO account" >&2; exit 1; }

doctl compute droplet create "$NAME" \
  --region "$REGION" --image "$IMAGE" --size "$SIZE" \
  --ssh-keys "$KEYS" --user-data-file cloud-init.yaml \
  --tag-name bough --wait --format ID,Name,PublicIPv4

ID=$(doctl compute droplet list --no-header --format ID,Name | awk -v n="$NAME" '$2==n{print $1}' | head -1)
IP=$(doctl compute droplet get "$ID" --no-header --format PublicIPv4)

doctl compute firewall create --name "$NAME-setup" \
  --inbound-rules "protocol:tcp,ports:22,address:$MYIP/32" \
  --outbound-rules "protocol:tcp,ports:all,address:0.0.0.0/0,address:::/0 protocol:udp,ports:all,address:0.0.0.0/0,address:::/0 protocol:icmp,address:0.0.0.0/0,address:::/0" \
  --droplet-ids "$ID" >/dev/null

echo "droplet=$ID ip=$IP"
