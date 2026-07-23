/**
 * Grafana (gcx) integration — `gcx` works from the sandbox without its tokens
 * ever entering it. The operator's `~/.config/gcx/config.yaml` holds per-context
 * server URLs + API tokens; we write a sanitized copy (tokens replaced by a
 * placeholder) that build-golden.sh bakes at the guest's own gcx config path,
 * and the proxy overwrites Authorization with the real token per server host
 * (same pattern as argocd/GitHub). Tokens are re-read per mint, so refreshing
 * one on the host takes effect without a server restart.
 */
import { parse, stringify } from "@std/yaml";
import { readFileSync } from "node:fs";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { netDir } from "./install.ts";
import type { CredentialRule } from "./proxy.ts";

/** Useless placeholder in the sandbox copy; fails closed without the MITM. */
export const GRAFANA_SENTINEL = "__bough_grafana_token__";

interface GcxConfig {
  "current-context"?: string;
  contexts?: Record<
    string,
    { grafana?: { server?: string; token?: string; [k: string]: unknown } }
  >;
}

/** The gcx CLI's local config path: $BOUGH_GCX_CONFIG, else the default. */
export function gcxConfigPath(): string {
  const override = Deno.env.get("BOUGH_GCX_CONFIG");
  if (override) return override;
  return join(Deno.env.get("HOME") ?? "", ".config", "gcx", "config.yaml");
}

function readConfig(path: string): GcxConfig | undefined {
  try {
    return parse(readFileSync(path, "utf8")) as GcxConfig;
  } catch {
    return undefined;
  }
}

/** Server hostname → token (first tokened context per host wins). */
function tokensByHost(cfg: GcxConfig): Map<string, string> {
  const out = new Map<string, string>();
  for (const ctx of Object.values(cfg.contexts ?? {})) {
    const g = ctx?.grafana;
    if (!g?.server || !g.token) continue;
    try {
      const host = new URL(g.server).hostname;
      if (!out.has(host)) out.set(host, g.token);
    } catch {
      // non-URL server value — skip
    }
  }
  return out;
}

export interface GcxSetup {
  /** Sanitized config the golden bakes at the guest gcx config path. */
  configPath: string;
  /** Grafana server hosts — trusted at the host gate, token-stamped. */
  hosts: string[];
  credentials: CredentialRule[];
}

/**
 * Read the operator's gcx config; write the sanitized sandbox copy to
 * <netDir>/gcx-config. Returns undefined when absent/unparseable/tokenless —
 * gcx then simply isn't set up (installed but unauthenticated).
 */
export function setupGcx(path = gcxConfigPath(), dir = netDir()): GcxSetup | undefined {
  const cfg = readConfig(path);
  if (!cfg) return undefined;
  const tokens = tokensByHost(cfg);
  if (tokens.size === 0) return undefined;

  const sanitized = structuredClone(cfg);
  for (const ctx of Object.values(sanitized.contexts ?? {})) {
    if (ctx?.grafana?.token) ctx.grafana.token = GRAFANA_SENTINEL;
  }
  const configPath = join(dir, "gcx-config");
  mkdirSync(dir, { recursive: true });
  writeFileSync(configPath, stringify(sanitized), { mode: 0o600 });

  const credentials: CredentialRule[] = [...tokens.keys()].map((host) => ({
    host,
    header: "authorization",
    value: () => {
      const fresh = readConfig(path);
      const token = fresh && tokensByHost(fresh).get(host);
      if (!token) return Promise.reject(new Error(`no gcx token for ${host}`));
      return Promise.resolve(`Bearer ${token}`);
    },
  }));
  return { configPath, hosts: [...tokens.keys()], credentials };
}
