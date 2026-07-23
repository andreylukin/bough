/**
 * AWS credential broker — the human-account half of bough's read-only AWS identity.
 *
 * Under IAM Identity Center the SSO cache lives in YOUR home and refreshes
 * interactively (`aws sso login`), so the agent user can't mint AWS credentials
 * itself. This tiny loopback server runs as a LaunchAgent in your account and
 * hands the agent short-lived, read-only credentials over the ECS
 * container-credentials protocol (AWS_CONTAINER_CREDENTIALS_FULL_URI):
 *
 *   GET /aws        read-only creds (profile bough-ro) — injected into the sandbox
 *   GET /aws-admin  your default-profile creds — host-side kube mint ONLY, never
 *                   injected; the k8s API demotes it via Impersonate-User (Phase 3)
 *
 * Both require the bearer token written to a bough-work-group-readable file at
 * boot, so only you and the agent user can read it. Creds are cached until just
 * before expiry with single-flight, and a stale SSO session yields 503 with the
 * exact `aws sso login` command to run.
 *
 *   deno run --allow-net --allow-env --allow-read --allow-write --allow-run \
 *     scripts/cred-broker.ts
 */
const PORT = Number(Deno.env.get("BOUGH_AWS_BROKER_PORT") ?? "9109");
const RO_PROFILE = Deno.env.get("BOUGH_AWS_RO_PROFILE") ?? "bough-ro";
const ADMIN_PROFILE = Deno.env.get("BOUGH_AWS_ADMIN_PROFILE") ?? "default";
const TOKEN_PATH = Deno.env.get("BOUGH_AWS_BROKER_TOKEN_FILE") ??
  `${Deno.env.get("HOME")}/.bough/broker-token`;
const GROUP = "bough-work";

/** Re-mint this long before the reported expiry (clock skew + latency). */
const EXPIRY_SLACK_MS = 60_000;
const DEFAULT_TTL_MS = 5 * 60_000;

/** A fresh bearer per boot, group-readable (0640) so only you + the agent can read it. */
async function installToken(): Promise<string> {
  const token = crypto.randomUUID() + crypto.randomUUID().replaceAll("-", "");
  await Deno.writeTextFile(TOKEN_PATH, token);
  await Deno.chmod(TOKEN_PATH, 0o640);
  // chgrp so the agent user (group member) can read it; best-effort.
  try {
    await new Deno.Command("chgrp", { args: [GROUP, TOKEN_PATH] }).output();
  } catch { /* group may not exist pre-cutover */ }
  return token;
}

interface Creds {
  AccessKeyId: string;
  SecretAccessKey: string;
  Token: string;
  Expiration: string; // ISO8601
}

export class SsoExpired extends Error {}

/** True when an `aws` stderr indicates the SSO session needs a re-login. */
export function isSsoExpired(stderr: string): boolean {
  const e = stderr.toLowerCase();
  return e.includes("sso") || e.includes("expired") || e.includes("token has expired");
}

/** Map `aws configure export-credentials --format process` JSON to container-cred JSON. */
export function toContainerCreds(processJson: string): Creds {
  const c = JSON.parse(processJson);
  // export-credentials calls it SessionToken; the container endpoint wants Token.
  return {
    AccessKeyId: c.AccessKeyId,
    SecretAccessKey: c.SecretAccessKey,
    Token: c.SessionToken,
    Expiration: c.Expiration ?? new Date(Date.now() + DEFAULT_TTL_MS).toISOString(),
  };
}

/** Run `aws configure export-credentials` for a profile; map to container-cred JSON. */
async function exportCredentials(profile: string): Promise<Creds> {
  const out = await new Deno.Command("aws", {
    args: ["configure", "export-credentials", "--profile", profile, "--format", "process"],
    stdout: "piped",
    stderr: "piped",
    signal: AbortSignal.timeout(30_000),
  }).output();
  if (out.code !== 0) {
    const stderr = new TextDecoder().decode(out.stderr);
    if (isSsoExpired(stderr)) throw new SsoExpired(profile);
    throw new Error(stderr.trim() || `aws exited ${out.code}`);
  }
  return toContainerCreds(new TextDecoder().decode(out.stdout));
}

/** Cache + single-flight per profile, modeled on src/net/execcred.ts. */
function provider(profile: string): () => Promise<Creds> {
  let cached: { creds: Creds; expires: number } | undefined;
  let inflight: Promise<Creds> | undefined;
  return () => {
    if (cached && Date.now() < cached.expires) return Promise.resolve(cached.creds);
    inflight ??= exportCredentials(profile)
      .then((creds) => {
        const reported = Date.parse(creds.Expiration);
        const expires = Number.isFinite(reported)
          ? reported - EXPIRY_SLACK_MS
          : Date.now() + DEFAULT_TTL_MS;
        cached = { creds, expires };
        return creds;
      })
      .finally(() => {
        inflight = undefined;
      });
    return inflight;
  };
}

/** Build the request handler for the broker; `get` maps a path to its cred provider. */
export function makeHandler(
  token: string,
  providers: Record<string, () => Promise<Creds>>,
  profileFor: Record<string, string>,
): (req: Request) => Promise<Response> {
  return async (req) => {
    const { pathname } = new URL(req.url);
    const get = providers[pathname];
    if (!get) return new Response("not found\n", { status: 404 });

    const auth = req.headers.get("authorization") ?? "";
    const bearer = auth.replace(/^Bearer\s+/i, "");
    if (bearer !== token) return new Response("unauthorized\n", { status: 401 });

    try {
      return Response.json(await get());
    } catch (e) {
      if (e instanceof SsoExpired) {
        return new Response(
          `AWS SSO session expired. Run: aws sso login --profile ${profileFor[pathname]}\n`,
          { status: 503 },
        );
      }
      return new Response(`credential error: ${(e as Error).message}\n`, { status: 500 });
    }
  };
}

if (import.meta.main) {
  const providers: Record<string, () => Promise<Creds>> = {
    "/aws": provider(RO_PROFILE),
    "/aws-admin": provider(ADMIN_PROFILE),
  };
  const profileFor: Record<string, string> = { "/aws": RO_PROFILE, "/aws-admin": ADMIN_PROFILE };
  const token = await installToken();
  // VM-mode guests reach the broker at the gate host IP, not loopback — bind
  // 0.0.0.0 via BOUGH_AWS_BROKER_BIND (requests stay bearer-token gated).
  const bind = Deno.env.get("BOUGH_AWS_BROKER_BIND") ?? "127.0.0.1";
  console.log(`[cred-broker] listening on ${bind}:${PORT}; token at ${TOKEN_PATH}`);
  Deno.serve({ port: PORT, hostname: bind }, makeHandler(token, providers, profileFor));
}
