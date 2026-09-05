#!/usr/bin/env bash
# Step 2 of 2: replace the setup firewall with one that has zero
# inbound rules. SSH and the UI both arrive over the tailnet, which is
# outbound-initiated and needs no public hole.
set -euo pipefail
NAME="${1:-bough}"
ID=$(doctl compute droplet list --no-header --format ID,Name | awk -v n="$NAME" '$2==n{print $1}' | head -1)
OLD=$(doctl compute firewall list --no-header --format ID,Name | awk -v n="$NAME-setup" '$2==n{print $1}' | head -1)

doctl compute firewall create --name "$NAME-closed" \
  --inbound-rules "" \
  --outbound-rules "protocol:tcp,ports:all,address:0.0.0.0/0,address:::/0 protocol:udp,ports:all,address:0.0.0.0/0,address:::/0 protocol:icmp,address:0.0.0.0/0,address:::/0" \
  --droplet-ids "$ID" >/dev/null
[ -n "$OLD" ] && doctl compute firewall delete "$OLD" -f
echo "public inbound closed for $NAME ($ID)"
