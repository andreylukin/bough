/**
 * Argo CD CLI integration — make `argocd` work from the sandbox in SERVER mode
 * without the auth token ever entering it. The operator logs in on the host as
 * usual (`argocd login <server> --sso`); the CLI stores a bearer token in
 * `~/.config/argocd/config`. The sandbox gets only ARGOCD_SERVER plus a useless
 * placeholder ARGOCD_AUTH_TOKEN, and the proxy overwrites Authorization with the
 * host token on the wire (same pattern as the GitHub PAT and EKS exec tokens).
 *
 * The token provider re-reads the config on every mint, so a host-side re-login
 * (SSO expiry) takes effect without a server restart. Server mode over
 * `--grpc-web` is plain HTTPS unary POSTs, which the MITM proxy handles; core
 * mode is NOT usable from the sandbox (its kube port-forward needs a SPDY
 * upgrade the proxy doesn't pass).
 */
import { parse } from "@std/yaml";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { CredentialRule } from "./proxy.ts";

/** Placeholder the sandbox holds; fails closed if the MITM is ever bypassed. */
export const ARGOCD_SENTINEL = "__bough_argocd_token__";

interface ArgocdLocalConfig {
  "current-context"?: string;
  contexts?: { name?: string; server?: string; user?: string }[];
  users?: { name?: string; "auth-token"?: string }[];
}

/** The argocd CLI's local config path: $BOUGH_ARGOCD_CONFIG, else the default. */
export function argocdConfigPath(): string {
  const override = Deno.env.get("BOUGH_ARGOCD_CONFIG");
  if (override) return override;
  return join(Deno.env.get("HOME") ?? "", ".config", "argocd", "config");
}

function readConfig(path: string): ArgocdLocalConfig | undefined {
  try {
    return parse(readFileSync(path, "utf8")) as ArgocdLocalConfig;
  } catch {
    return undefined;
  }
}

/** Server host → its auth token, from the contexts/users join. */
function tokensByServer(cfg: ArgocdLocalConfig): Map<string, string> {
  const tokenByUser = new Map<string, string>();
  for (const u of cfg.users ?? []) {
    if (u.name && u["auth-token"]) tokenByUser.set(u.name, u["auth-token"]);
  }
  const out = new Map<string, string>();
  for (const c of cfg.contexts ?? []) {
    const token = c.user && tokenByUser.get(c.user);
    if (c.server && token && !out.has(c.server)) out.set(c.server, token);
  }
  return out;
}

export interface ArgocdSetup {
  /** The current-context server (what ARGOCD_SERVER points the sandbox at). */
  server: string;
  /** Every server with a token — trusted at the host gate, stamped by the proxy. */
  hosts: string[];
  /** Authorization rules: token re-read from the host config per mint. */
  credentials: CredentialRule[];
}

/**
 * Read the operator's argocd config. Returns undefined when absent, unparseable,
 * or holding no token — argocd then simply isn't set up in the sandbox.
 */
export function setupArgocd(path = argocdConfigPath()): ArgocdSetup | undefined {
  const cfg = readConfig(path);
  if (!cfg) return undefined;
  const tokens = tokensByServer(cfg);
  if (tokens.size === 0) return undefined;
  // current-context is a context NAME; resolve it to that context's server.
  const cur = (cfg.contexts ?? []).find((c) => c.name === cfg["current-context"]);
  const server = cur?.server && tokens.has(cur.server) ? cur.server : [...tokens.keys()][0];
  const credentials: CredentialRule[] = [...tokens.keys()].map((host) => ({
    host,
    header: "authorization",
    value: () => {
      const fresh = readConfig(path);
      const token = fresh && tokensByServer(fresh).get(host);
      if (!token) return Promise.reject(new Error(`no argocd token for ${host} — argocd login`));
      return Promise.resolve(`Bearer ${token}`);
    },
  }));
  return { server, hosts: [...tokens.keys()], credentials };
}
