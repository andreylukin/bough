/**
 * The single host address a sandbox VM guest reaches the Claw Patrol gate (and
 * the AWS creds broker) at — and the address the VM's `--allow-cidr` lockdown is
 * pinned to. It is one value used in three places that MUST agree: the proxy's
 * bind host, the `machine create --allow-cidr <ip>/32`, and envFor's proxy URL.
 *
 * Under smolvm TSI the guest routes to the host at the host's own LAN IP, so that
 * is the default. Override with BOUGH_GATE_HOST when the LAN IP isn't reachable
 * from the guest (e.g. a host firewall drops it — then point this at an address
 * the guest can reach, such as smolvm's per-guest gateway).
 */
export function gateHostIp(): string {
  const override = Deno.env.get("BOUGH_GATE_HOST");
  if (override) return override;
  const v4 = Deno.networkInterfaces().filter(
    (ni) => ni.family === "IPv4" && !ni.address.startsWith("127."),
  );
  // Prefer a primary Ethernet/Wi-Fi interface (en0, en1, …) over virtual ones.
  const primary = v4.find((ni) => /^en\d/.test(ni.name));
  return (primary ?? v4[0])?.address ?? "127.0.0.1";
}
