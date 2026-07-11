# Design: transparent egress via `NETransparentProxyProvider`

Status: **proposed** — not built. This documents what it would take to replace
bough's `HTTP_PROXY`-env + Seatbelt-loopback-confinement egress model with a macOS
Network Extension, why it's worth it, and the concrete integration points in the
current code.

## Why

Today the sandbox is pointed at Claw Patrol by exporting `HTTP_PROXY`/`HTTPS_PROXY`
into each shelled command, and Seatbelt denies all non-loopback egress so the proxy
is the only way out (see `src/sandbox/seatbelt.ts` `confineNetwork`, `src/net/gateway.ts`
`envFor`). This works, but it leaks a recurring class of bug: **tools that ignore proxy
env vars**.

- `argocd` ignores `HTTP_PROXY` for its gRPC transport (documented; see the `/argocd`
  skill — core mode is the workaround).
- Go binaries need `NODE_USE_ENV_PROXY`-style opt-ins or ignore the env entirely;
  the macOS keychain/CA quirks compound it (see memory: clawpatrol CA keychain).
- Anything statically honoring only `ALL_PROXY`, or nothing, silently bypasses — and
  under `confineNetwork` that becomes a hard connection failure the agent must debug.

`NETransparentProxyProvider` removes the env var from the equation: the OS redirects a
process's TCP flows to our provider regardless of the process's own proxy config. No
`HTTP_PROXY`, no per-tool opt-in, no "does this CLI honor the env" question ever again.

## What it is

A macOS **system extension** implementing `NETransparentProxyProvider`. Configured with
`NETransparentProxyNetworkSettings`, the OS calls `handleNewFlow(_:)` for each outbound
flow; the provider decides per-flow to either proxy it (relay through Claw Patrol) or
`return false` to let it proceed normally. It sees the flow's remote endpoint and owning
process before a single byte leaves — attribution and gating move into the OS.

Crucially this is **redirection**, which Seatbelt cannot do: Seatbelt only *denies*
(`src/sandbox/seatbelt.ts` is a deny mechanism). So the two compose — Seatbelt keeps
confining the filesystem; the NE takes over network steering.

## Hard prerequisites (the reason this is a project, not a patch)

1. **Apple Developer Program membership** and the
   `com.apple.developer.networking.networkextension` entitlement with the
   `transparent-proxy-provider` capability. This entitlement is not free-tier.
2. **A signed, notarized app bundle** that hosts the system extension (`.systemextension`
   inside `Contents/Library/SystemExtensions`). bough currently ships as a Deno binary /
   `deno desktop` app — this needs a real signed `.app` container.
3. **A Swift/ObjC target.** The provider is a `NEAppProxyProvider` subclass; there is no
   pure-Deno path. New build target, new language in the repo.
4. **User consent at install:** the system extension activation and the network config
   both prompt the user (System Settings → Login Items & Extensions). First-run UX and a
   fallback for "user declined" are required — likely fall back to today's env-var model.
5. **A privileged helper or XPC channel** so the Deno server (Claw Patrol policy owner)
   and the sandboxed extension exchange verdicts. The extension process is separate and
   sandboxed by the OS; it can't call into the Deno gate directly.

## Integration points in the current code

The gate/proxy layer is already the right shape to sit behind an NE — the change is in
*how flows arrive*, not *how they're judged*.

- **`src/net/proxy.ts`** — `ProxyServer` already terminates TLS (MITM), runs the gate,
  re-originates, and stamps credentials. An NE would hand it flows via a local socket
  instead of the CONNECT/HTTP-proxy front door. The `gate`, `credentials`, and
  `upstreamCa` wiring is unchanged. Keep it; feed it differently.
- **`src/net/gateway.ts`** `envFor` — the `HTTP_PROXY`/`NO_PROXY`/CA env block goes away
  for NE-steered processes. `caEnv` (trusting the MITM CA) **stays**: MITM still happens,
  so clients still need to trust bough's CA — the NE removes the *proxy env*, not the
  *cert trust*. (This is the same tradeoff gondolin makes: transparent steering, but the
  CA still has to be trusted at a known location.)
- **`src/sandbox/seatbelt.ts`** — `confineNetwork` (the loopback-only clamp) is no longer
  the egress backstop; the NE steers all flows by default. Seatbelt keeps its filesystem
  role. The `network-bind`/`network-inbound` loopback rules we added for local daemons
  stay relevant.
- **`src/net/credentials.ts`** (this change) — unaffected. Credential injection is a
  property of the proxy, independent of how flows reach it. This is the point of keeping
  credentials decoupled from the transport: swapping the front door doesn't touch them.

## What it does NOT fix

- **MITM is still MITM.** The NE removes the proxy-env-var bug class, not the CA-trust
  requirement. Tools that pin certificates or reject unknown roots still break; the
  keychain-trust setup for the MITM CA is still needed.
- **Non-TCP / QUIC.** `NETransparentProxyProvider` handles TCP (and UDP) flows; HTTP/3
  over QUIC and other UDP protocols need explicit handling and may be simplest to just
  deny (forcing TCP fallback), matching today's posture.
- **Linux.** This is macOS-only. The Linux story stays bubblewrap + netns + proxy env
  (which is also what Anthropic's `sandbox-runtime` uses cross-platform), so the env-var
  path can't be deleted — only bypassed on macOS.

## Recommendation

Sequence, if pursued:

1. Prototype the extension in a throwaway signed `.app` that steers flows to a
   hard-coded local port and logs `handleNewFlow` — prove the entitlement + consent flow
   on the target Macs.
2. Define the XPC verdict channel between the Deno gate and the extension.
3. Make the NE front-end optional and env-var the fallback, so a declined consent or a
   Linux host degrades to today's behavior with no feature loss.

Effort is dominated by (1) Apple signing/entitlement logistics and (5) the privileged
helper — both one-time. The gate/proxy/credential core is reused as-is.

## Alternatives considered

- **CONNECT-only (Smokescreen-style), no MITM.** Removes all CA/keychain nonsense but
  gives up path/body-level verdicts (the Claw Patrol verb gate) — a product regression.
  Rejected: the verb gate is the differentiator.
- **gondolin-style micro-VM userspace netstack.** Transparent steering + MITM with the CA
  at a fixed guest path; tested previously as a fit (see memory: gondolin network sandbox
  fit). Heavier (re-platforms execution into a VM) than a native NE for the macOS case.
