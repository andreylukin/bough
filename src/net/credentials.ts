/**
 * Credential registry — the one place that turns declared credential sources into the
 * proxy's `CredentialRule`s (proxy.ts). Two producers feed it today:
 *   - bundle bindings (config.credentials): `{host, header, env}` — the token lives in
 *     bough's own environment under `env`; only the NAME is persisted. Read at request
 *     time so a rotated env value takes effect without reinstalling the bundle.
 *   - kube exec plugins (cloud.ts KubeSetup.credentials): already `CredentialRule`s with
 *     host-side minting providers — passed through unchanged.
 *
 * The proxy is the sole credential holder; the sandbox never sees a token. An env var
 * named by a binding but unset surfaces as a 502 "credential mint failed" (the provider
 * throws — proxy.ts), so a missing token says so instead of a silent unauthenticated
 * request that 401s at the origin.
 *
 * This registry is why a bundle's `credential` block is no longer decorative: install a
 * bundle with a token env var and its writes are actually authenticated on the wire.
 */
import type { NetConfig } from "./config.ts";
import type { CredentialRule } from "./proxy.ts";

/** Resolve config credential bindings into proxy CredentialRules (env read per request). */
export function bindingRules(bindings: NetConfig["credentials"]): CredentialRule[] {
  return bindings.map((b) => ({
    host: b.host,
    header: b.header,
    value: () => {
      const token = Deno.env.get(b.env);
      if (!token) {
        return Promise.reject(
          new Error(`env ${b.env} is unset (the token for ${b.host} lives in bough's environment)`),
        );
      }
      return Promise.resolve((b.template ?? "Bearer {token}").replaceAll("{token}", token));
    },
  }));
}

/**
 * The full credential rule set for a session's proxy: bundle bindings from the
 * resolved config, then the kube exec creds. Order matters only if two rules claim
 * the same host+header — later wins (proxy.ts stamps in order), so kube (more specific,
 * host-minted) is placed last deliberately.
 */
export function resolveCredentials(
  config: NetConfig,
  kube?: readonly CredentialRule[],
): CredentialRule[] {
  return [...bindingRules(config.credentials), ...(kube ?? [])];
}
