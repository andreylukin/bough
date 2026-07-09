/**
 * Host-side exec credential minting for the kubectl MITM path. A kubeconfig's exec
 * plugin (`aws eks get-token`, `gke-gcloud-auth-plugin`, ...) can't run in the
 * sandbox — it reads credential stores the seatbelt denies (~/.aws, ~/.config/gcloud).
 * So the bough server runs it here, with its own (unsandboxed) environment, and the
 * proxy stamps the minted bearer token onto requests to the cluster host
 * (proxy.ts CredentialRule). The AWS/GCP secrets never enter the sandbox; the only
 * thing that crosses the boundary is a short-lived, cluster-scoped k8s token — and
 * it crosses inside the proxy, not the sandbox.
 *
 * Output contract: the plugin prints a client.authentication.k8s.io ExecCredential
 * JSON — `{ status: { token, expirationTimestamp? } }`. Tokens are cached until
 * shortly before expiry (or a conservative default when the plugin doesn't say),
 * with single-flight so a burst of requests mints once.
 */
import type { ExecCredSpec } from "./kubeconfig.ts";

/** Re-mint this long before the reported expiry (clock skew + request latency). */
const EXPIRY_SLACK_MS = 60_000;
/** Cache lifetime when the plugin reports no expirationTimestamp. */
const DEFAULT_TTL_MS = 5 * 60_000;

async function mint(spec: ExecCredSpec): Promise<{ header: string; expires: number }> {
  let out: Deno.CommandOutput;
  try {
    out = await new Deno.Command(spec.command, {
      args: spec.args,
      env: spec.env, // merged over the server's own env (Deno default), where ~/.aws etc. live
      stdout: "piped",
      stderr: "piped",
      signal: AbortSignal.timeout(30_000),
    }).output();
  } catch (e) {
    throw new Error(`${spec.command} did not run: ${(e as Error).message}`);
  }
  const stderr = new TextDecoder().decode(out.stderr).trim();
  if (out.code !== 0) {
    // Surface the plugin's own message — "sso session expired" etc. is the fix hint.
    throw new Error(`${spec.command} exited ${out.code}: ${stderr.slice(-300) || "(no stderr)"}`);
  }
  let token: string | undefined;
  let expiry: string | undefined;
  try {
    const cred = JSON.parse(new TextDecoder().decode(out.stdout));
    token = cred?.status?.token;
    expiry = cred?.status?.expirationTimestamp;
  } catch {
    // fall through to the !token error
  }
  if (typeof token !== "string" || !token) {
    throw new Error(`${spec.command} printed no ExecCredential token`);
  }
  const reported = expiry ? Date.parse(expiry) : NaN;
  const expires = Number.isFinite(reported)
    ? reported - EXPIRY_SLACK_MS
    : Date.now() + DEFAULT_TTL_MS;
  return { header: `Bearer ${token}`, expires };
}

/**
 * A CredentialRule value provider for one cluster's exec plugin: returns the
 * `Authorization` header value, minting on first use and re-minting after expiry.
 */
export function execBearerProvider(spec: ExecCredSpec): () => Promise<string> {
  let cached: { header: string; expires: number } | undefined;
  let inflight: Promise<string> | undefined;
  return () => {
    if (cached && Date.now() < cached.expires) return Promise.resolve(cached.header);
    inflight ??= mint(spec)
      .then((m) => {
        cached = m;
        return m.header;
      })
      .finally(() => {
        inflight = undefined;
      });
    return inflight;
  };
}
