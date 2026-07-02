/**
 * Policy bundles — installable, publishable Claw Patrol policy fragments for a
 * service or tool (design doc: "Policy bundles"). A bundle is an HCL template +
 * manifest: typed params, a description, required credential handles, and test
 * fixtures. `bough net add <bundle>` surfaces params as a form, calls `render()`,
 * composes the fragment into the gateway HCL, and runs `clawpatrol test` against
 * the fixtures before hot-reloading.
 *
 * This file is the interface + one worked example (`github`). Other bundles
 * (`aws-readonly`, `kubernetes-prod`, ...) implement the same shape.
 */

import type { HclCredential, HclEndpoint, HclRule } from "./hcl.ts";
import type { Verdict } from "./policy.ts";

export type ParamType = "string" | "host" | "hostList" | "bool";

export interface BundleParam {
  name: string;
  description: string;
  type: ParamType;
  required: boolean;
  default?: string | string[] | boolean;
}

/** A credential the bundle needs; the value is injected by the gateway, never held by the agent. */
export interface BundleCred {
  handle: string; // e.g. "github_pat"
  type: string; // e.g. "bearer_token"
  description: string;
}

/** One offline regression case, matching Claw Patrol's `clawpatrol test` fixture JSON. */
export interface BundleFixture {
  name: string;
  action: Record<string, unknown>; // { host, http|k8s|sql: {...} }
  expect: { verdict: Verdict; rule?: string; endpoint?: string };
}

/** The HCL fragment a bundle contributes, composed into the full gateway policy. */
export interface BundleFragment {
  credentials: HclCredential[];
  endpoints: HclEndpoint[];
  rules: HclRule[];
}

export interface BundleManifest {
  name: string;
  version: string;
  description: string;
  params: BundleParam[];
  credentials: BundleCred[];
  fixtures: BundleFixture[];
  /** Compose this bundle's HCL from resolved params. */
  render(params: Record<string, unknown>): BundleFragment;
}

// ---- example bundle: github -------------------------------------------------

/**
 * GitHub: REST gated by method, GraphQL gated by the operation in the body (gh
 * sends most commands as POST /graphql, so method alone can't tell read from
 * write). First-match wins, so the graphql rules precede the REST rules. Mirrors
 * the spike's `github-*` rules and policy.ts's `classifyGithub`.
 */
export const githubBundle: BundleManifest = {
  name: "github",
  version: "0.1.0",
  description: "Gate gh/GitHub API: reads allowed, writes reviewed (deny by default).",
  params: [
    {
      name: "host",
      description: "GitHub API host (github.com for GHES: api.ghe.example).",
      type: "host",
      required: false,
      default: "api.github.com",
    },
    {
      name: "allowWrites",
      description: "Permit REST/GraphQL writes instead of denying them.",
      type: "bool",
      required: false,
      default: false,
    },
  ],
  credentials: [
    {
      handle: "github_pat",
      type: "bearer_token",
      description: "GitHub personal-access / OAuth token, stamped on the wire by the gateway.",
    },
  ],
  fixtures: [
    {
      name: "gh-get",
      action: { host: "api.github.com", http: { method: "GET", path: "/user", headers: {} } },
      expect: { verdict: "allow", rule: "github-rest-reads", endpoint: "https.github" },
    },
    {
      name: "gh-delete",
      action: { host: "api.github.com", http: { method: "DELETE", path: "/repos/o/r", headers: {} } },
      expect: { verdict: "deny", rule: "github-rest-writes", endpoint: "https.github" },
    },
    {
      name: "gh-graphql-query",
      action: {
        host: "api.github.com",
        http: { method: "POST", path: "/graphql", headers: {}, body: '{"query":"query { viewer { login } }"}' },
      },
      expect: { verdict: "allow", rule: "github-graphql-query", endpoint: "https.github" },
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
      expect: { verdict: "deny", rule: "github-graphql-mutation", endpoint: "https.github" },
    },
  ],
  render(params) {
    const host = (params.host as string) ?? "api.github.com";
    const writeVerdict: Verdict = params.allowWrites ? "allow" : "deny";
    return {
      credentials: [{ type: "bearer_token", name: "github_pat", endpoint: "https.github" }],
      endpoints: [{ kind: "https", name: "github", hosts: [host] }],
      rules: [
        {
          name: "github-graphql-mutation",
          endpoint: "https.github",
          condition: "http.path == '/graphql' && http.body_json.query.startsWith('mutation')",
          verdict: writeVerdict,
          reason: "graphql mutation blocked",
        },
        {
          name: "github-graphql-query",
          endpoint: "https.github",
          condition: "http.path == '/graphql'",
          verdict: "allow",
        },
        {
          name: "github-rest-reads",
          endpoint: "https.github",
          condition: "http.method in ['GET', 'HEAD']",
          verdict: "allow",
        },
        {
          name: "github-rest-writes",
          endpoint: "https.github",
          condition: "http.method in ['POST', 'PATCH', 'PUT', 'DELETE']",
          verdict: writeVerdict,
          reason: "REST writes go through PR review",
        },
      ],
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
