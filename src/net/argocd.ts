/**
 * Argo CD integration — the sandbox path for `argocd` is CORE mode
 * (`argocd --core`): it rides the kube API through the proxy, including the
 * SPDY port-forward via the Upgrade passthrough (proxy.ts #upgrade). The
 * argocd CLI's own server mode is a dead end in the sandbox — its transport
 * ignores HTTPS_PROXY, dials the origin directly, and the egress lockdown
 * refuses it — so no ARGOCD_* env is injected.
 *
 * This module additionally trusts the operator's argocd server hosts and
 * stamps their Authorization from the host `~/.config/argocd/config` token
 * (same pattern as the GitHub PAT), so raw HTTPS (curl) against the argocd
 * REST API works from the sandbox without the token ever entering it. The
 * token is re-read per mint, so a host-side re-login (`argocd login --sso`)
 * takes effect without a server restart.
 */
import { parse } from "@std/yaml";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { CredentialRule } from "./proxy.ts";

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
