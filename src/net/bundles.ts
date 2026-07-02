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

import type { Verdict } from "./policy.ts";

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

/** What a bundle contributes to the rule set — merged (deduped) into the persisted NetConfig. */
export interface BundleContribution {
  allowHosts?: string[];
  denyHosts?: string[];
  k8sHosts?: string[];
  allowVerbs?: string[];
  denyVerbs?: string[];
  holdVerbs?: string[];
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
 * incl. GraphQL by peeking for a `mutation`). Adds an explicit hold on graphql mutations
 * so they always surface for approval, even under mode "all".
 */
export const githubBundle: BundleManifest = {
  name: "github",
  version: "0.2.0",
  description: "Trust the GitHub API: reads pass, writes gated, graphql mutations held.",
  params: [
    {
      name: "host",
      description: "GitHub API host (github.com for GHES: api.ghe.example).",
      type: "host",
      required: false,
      default: "api.github.com",
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
      expect: { verdict: "hold" },
    },
  ],
  render(params) {
    const host = (params.host as string) ?? "api.github.com";
    return {
      allowHosts: [host],
      holdVerbs: ["graphql:mutation"],
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
