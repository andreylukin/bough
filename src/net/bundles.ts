/**
 * Policy bundles — installable, curated rule-set presets for a service or tool. A
 * bundle is a manifest (typed params, description, credential handles, test fixtures)
 * plus a `render()` that produces a NetConfig contribution: the hosts to trust and any
 * explicit verb rules. `bough net add <bundle>` surfaces params as a form, renders the
 * contribution, validates it against the bundle's fixtures (policy.ts), and merges it
 * into the live rule set (config.ts) — the gate hot-swaps.
 *
 * This file is the interface + one worked example (`github`). Other bundles
 * (`aws-readonly`, `kubernetes-prod`, ...) implement the same shape.
 */

import type { Rule, Verdict } from "./policy.ts";

export type ParamType = "string" | "host" | "hostList" | "bool";

export interface BundleParam {
  name: string;
  description: string;
  type: ParamType;
  required: boolean;
  default?: string | string[] | boolean;
}

/** A credential the bundle needs; the value is injected by the proxy, never held by the agent. */
export interface BundleCred {
  handle: string; // e.g. "github_pat"
  type: string; // e.g. "bearer_token"
  description: string;
}

/** One offline regression case: an action and the verdict the composed policy must give it. */
export interface BundleFixture {
  name: string;
  action: Record<string, unknown>; // { host, http|k8s|sql: {...} }
  expect: { verdict: Verdict; rule?: string; endpoint?: string };
}

/**
 * A credential binding a bundle contributes: stamp `header` on requests to `host`,
 * with the value read from bough's env var `env` at request time. Only the var NAME
 * is persisted — the secret itself never touches config or the sandbox; the proxy
 * resolves it host-side (see credentials.ts). Matches the MCP `${VAR}` convention.
 */
export interface CredentialBinding {
  host: string; // exact host or "*.suffix"
  header: string; // e.g. "authorization"
  /** Env var in bough's own environment holding the token; the value is `Bearer <it>`. */
  env: string;
  /** Header value template: `{token}` is replaced with the env value. Default "Bearer {token}". */
  template?: string;
}

/** What a bundle contributes to the rule set — merged (deduped) into the persisted NetConfig. */
export interface BundleContribution {
  allowHosts?: string[];
  denyHosts?: string[];
  k8sHosts?: string[];
  /** Condition rules (policy.ts Rule) — merged by name, bundle order preserved. */
  rules?: Rule[];
  allowVerbs?: string[];
  denyVerbs?: string[];
  holdVerbs?: string[];
  /** Credential injections — the proxy stamps these host-side (credentials.ts). */
  credentials?: CredentialBinding[];
}

export interface BundleManifest {
  name: string;
  version: string;
  description: string;
  params: BundleParam[];
  credentials: BundleCred[];
  fixtures: BundleFixture[];
  /** Compose this bundle's rule-set contribution from resolved params. */
  render(params: Record<string, unknown>): BundleContribution;
}

// ---- example bundle: github -------------------------------------------------

/**
 * GitHub: trust the API host so reads pass and writes are gated by the active mode
 * (review holds them, read_only denies them — the classifier splits GitHub read/write,
 * incl. GraphQL by peeking for a `mutation`). Contributes a condition RULE holding
 * graphql mutations so they always surface for approval, even under mode "all".
 */
export const githubBundle: BundleManifest = {
  name: "github",
  version: "0.3.0",
  description: "Trust the GitHub API: reads pass, writes gated, graphql mutations held.",
  params: [
    {
      name: "host",
      description: "GitHub API host (github.com for GHES: api.ghe.example).",
      type: "host",
      required: false,
      default: "api.github.com",
    },
    {
      name: "tokenEnv",
      description:
        "Name of the bough env var holding the GitHub token. Empty → no injection " +
        "(reads still flow; writes just aren't authenticated by the proxy).",
      type: "string",
      required: false,
      default: "",
    },
  ],
  credentials: [
    {
      handle: "github_pat",
      type: "bearer_token",
      description: "GitHub personal-access / OAuth token, stamped on the wire by the proxy.",
    },
  ],
  fixtures: [
    {
      name: "gh-get",
      action: { host: "api.github.com", http: { method: "GET", path: "/user", headers: {} } },
      expect: { verdict: "allow" },
    },
    {
      name: "gh-delete",
      action: {
        host: "api.github.com",
        http: { method: "DELETE", path: "/repos/o/r", headers: {} },
      },
      expect: { verdict: "deny" },
    },
    {
      name: "gh-graphql-query",
      action: {
        host: "api.github.com",
        http: {
          method: "POST",
          path: "/graphql",
          headers: {},
          body: '{"query":"query { viewer { login } }"}',
        },
      },
      expect: { verdict: "allow" },
    },
    {
      name: "gh-graphql-mutation",
      action: {
        host: "api.github.com",
        http: {
          method: "POST",
          path: "/graphql",
          headers: {},
          body: '{"query":"mutation { mergePullRequest(input:{}) { clientMutationId } }"}',
        },
      },
      expect: { verdict: "hold", rule: "github-graphql-mutation" },
    },
    {
      name: "git-fetch",
      action: {
        host: "github.com",
        http: { method: "POST", path: "/o/r.git/git-upload-pack", headers: {} },
      },
      expect: { verdict: "allow", rule: "github-git-fetch" },
    },
    {
      name: "git-push",
      action: {
        host: "github.com",
        http: { method: "POST", path: "/o/r.git/git-receive-pack", headers: {} },
      },
      expect: { verdict: "hold", rule: "github-git-push" },
    },
  ],
  render(params) {
    const host = (params.host as string) ?? "api.github.com";
    const tokenEnv = ((params.tokenEnv as string) ?? "").trim();
    // git-over-HTTPS uses the web host (github.com), not the API host. Derive it so
    // clone/fetch/push traverse the gate too; for GHES api.ghe.x → ghe.x.
    const gitHost = host === "api.github.com" ? "github.com" : host.replace(/^api\./, "");
    const hosts = [...new Set([host, gitHost])];
    return {
      allowHosts: hosts,
      rules: [
        {
          name: "github-graphql-mutation",
          condition: "action.verb == 'graphql:mutation'",
          verdict: "hold",
          reason: "graphql mutation needs approval",
        },
        // git smart-HTTP: fetch/clone (git-upload-pack) is a read — allow it so it's
        // frictionless despite the POST. Push (git-receive-pack) always holds, both the
        // POST and the info/refs advertisement that precedes it (query carries service).
        {
          name: "github-git-fetch",
          condition: "http.path.endsWith('/git-upload-pack')",
          verdict: "allow",
          reason: "git fetch/clone (read)",
        },
        {
          name: "github-git-push",
          condition: "http.path.endsWith('/git-receive-pack') || " +
            "(has(http.query.service) && http.query.service == 'git-receive-pack')",
          verdict: "hold",
          reason: "git push needs approval",
        },
      ],
      // Inject the token host-side when the operator named an env var — on BOTH the API
      // and git hosts, so gh and git are authenticated on the wire. The secret stays in
      // bough's environment; only its NAME rides in the persisted config.
      ...(tokenEnv
        ? { credentials: hosts.map((h) => ({ host: h, header: "authorization", env: tokenEnv })) }
        : {}),
    };
  },
};

// ---- registry ---------------------------------------------------------------
// The installable bundles bough knows about. A registry (JSR / git index) feeds this
// later; for now it's the built-ins, keyed by name. Add a bundle = one line here.

export const bundles = new Map<string, BundleManifest>([
  [githubBundle.name, githubBundle],
]);

export function listBundles(): BundleManifest[] {
  return [...bundles.values()];
}

export function getBundle(name: string): BundleManifest | undefined {
  return bundles.get(name);
}
