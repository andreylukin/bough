/**
 * GitHub host-auth integration — sessions get the operator's own `gh` login
 * without a token ever entering the sandbox (same pattern as argocd/gcx). At
 * gateway init we probe `gh auth token`; when the host is logged in, the proxy
 * stamps that token on github.com (git-over-HTTPS wants Basic) and
 * api.github.com (the REST/GraphQL API), and the guest gets the GH_TOKEN
 * sentinel so `gh` sends an authenticated request shape at all. Tokens are
 * re-read per mint (short cache), so a host-side `gh auth login`/refresh takes
 * effect without a server restart. An explicit github bundle binding still
 * wins: the gateway registers these rules first and the proxy stamps in order.
 */
import type { CredentialRule } from "./proxy.ts";

/** launchd services miss the interactive PATH; try the usual brew prefixes too. */
const EXTRA_BIN_DIRS = ["/opt/homebrew/bin", "/usr/local/bin"];

/** Absolute path to the gh binary, or undefined when not installed. */
function ghBin(): string | undefined {
  const dirs = [...(Deno.env.get("PATH")?.split(":") ?? []), ...EXTRA_BIN_DIRS];
  for (const dir of dirs) {
    if (!dir) continue;
    try {
      const path = `${dir}/gh`;
      if (Deno.statSync(path).isFile) return path;
    } catch {
      // not here — keep looking
    }
  }
  return undefined;
}

/** Runs `gh auth token`; injectable so tests need no binary. */
export type GhTokenRun = () => Promise<string | undefined>;

const defaultRun: GhTokenRun = async () => {
  const bin = ghBin();
  if (!bin) return undefined;
  try {
    const out = await new Deno.Command(bin, {
      args: ["auth", "token", "--hostname", "github.com"],
      stdout: "piped",
      stderr: "null",
    }).output();
    if (!out.success) return undefined;
    return new TextDecoder().decode(out.stdout).trim() || undefined;
  } catch {
    return undefined;
  }
};

/** `gh auth token` prints without a network round-trip, but don't subprocess per request. */
const TOKEN_TTL_MS = 60_000;

export interface GithubSetup {
  credentials: CredentialRule[];
}

/**
 * Probe the host's gh login; undefined when gh is absent or unauthenticated —
 * github then simply isn't credentialed (anonymous requests flow unstamped).
 * Expiry AFTER init surfaces as a github 401, exactly like a stale PAT binding.
 */
export async function setupGithub(run: GhTokenRun = defaultRun): Promise<GithubSetup | undefined> {
  if (!(await run())) return undefined;

  let cached: { token: string; at: number } | undefined;
  let inflight: Promise<string> | undefined;
  const token = (): Promise<string> => {
    if (cached && Date.now() - cached.at < TOKEN_TTL_MS) return Promise.resolve(cached.token);
    inflight ??= (async () => {
      try {
        const t = await run();
        if (!t) throw new Error("gh auth token returned nothing — run `gh auth login` on the host");
        cached = { token: t, at: Date.now() };
        return t;
      } finally {
        inflight = undefined;
      }
    })();
    return inflight;
  };

  return {
    credentials: [
      {
        // git smart HTTP: github accepts the oauth token only as Basic (the
        // username is ignored; x-access-token is gh's own credential-helper shape).
        host: "github.com",
        header: "authorization",
        value: async () => `Basic ${btoa(`x-access-token:${await token()}`)}`,
      },
      {
        host: "api.github.com",
        header: "authorization",
        value: async () => `token ${await token()}`,
      },
    ],
  };
}
