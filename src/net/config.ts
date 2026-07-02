/**
 * The configurable rule set — bough's editable egress policy, persisted as
 * ~/.bough/net/policy.json and compiled into the runtime Policy the gate enforces
 * (policy.ts). This is the "configurable sets of network calls that are auto
 * allowed / denied / held" surface: the Network rail's rule editor reads it via
 * GET /net/policy and writes it via PUT /net/policy, which hot-swaps the live gate.
 *
 * The seeded default is fail-closed-but-approvable: reads pass, writes are HELD for
 * approval (mode "review"), a curated dev-host allowlist is trusted, and any host off
 * the allowlist is held rather than silently allowed. Editing any field is one PUT.
 */
import { z } from "zod/v4";
import { join } from "node:path";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { type Policy, policy } from "./policy.ts";
import { netDir } from "./install.ts";

const Verdict = z.enum(["allow", "deny", "hold"]);

export const NetConfig = z.object({
  /** Baseline action gate for allowed hosts: read_only (writes deny) | review (writes hold) | all. */
  mode: z.enum(["read_only", "review", "all"]).default("review"),
  /** Trusted hosts. A request to a host NOT here gets `hostMiss`. Empty = every host allowed (sniff-only). */
  allowHosts: z.array(z.string()).default([]),
  /** Hosts denied outright (win over allowHosts). */
  denyHosts: z.array(z.string()).default([]),
  /** What to do with a host that misses a non-empty allowHosts: fail closed (deny), ask (hold), or allow. */
  hostMiss: Verdict.default("hold"),
  /** API-server hosts classified as kubernetes (verb = HTTP method). */
  k8sHosts: z.array(z.string()).default([]),
  /** Explicit per-action overrides by classified verb (e.g. "DELETE /repos/o/r", "graphql:mutation"). */
  allowVerbs: z.array(z.string()).default([]),
  denyVerbs: z.array(z.string()).default([]),
  holdVerbs: z.array(z.string()).default([]),
});
export type NetConfig = z.infer<typeof NetConfig>;

/** A curated read-mostly dev allowlist (GitHub + the major package registries). Fully user-editable. */
const SEED_HOSTS = [
  "github.com",
  "api.github.com",
  "codeload.github.com",
  "objects.githubusercontent.com",
  "raw.githubusercontent.com",
  "registry.npmjs.org",
  "pypi.org",
  "files.pythonhosted.org",
  "crates.io",
  "static.crates.io",
  "proxy.golang.org",
  "sum.golang.org",
  "deno.land",
  "jsr.io",
  "esm.sh",
];

/** The seeded fail-closed-but-approvable posture. */
export function defaultConfig(): NetConfig {
  return NetConfig.parse({
    mode: "review",
    allowHosts: SEED_HOSTS,
    hostMiss: "hold",
  });
}

function configPath(dir = netDir()): string {
  return join(dir, "policy.json");
}

/** Read the persisted config; seed + persist the default on first run. Bad JSON falls back to default. */
export function loadConfig(dir = netDir()): NetConfig {
  const path = configPath(dir);
  if (!existsSync(path)) {
    const seeded = defaultConfig();
    writeFileSync(path, JSON.stringify(seeded, null, 2));
    return seeded;
  }
  try {
    return NetConfig.parse(JSON.parse(readFileSync(path, "utf8")));
  } catch {
    return defaultConfig();
  }
}

export function saveConfig(cfg: NetConfig, dir = netDir()): NetConfig {
  const parsed = NetConfig.parse(cfg);
  writeFileSync(configPath(dir), JSON.stringify(parsed, null, 2));
  return parsed;
}

/** Compile the editable config into the runtime Policy the gate enforces. */
export function toPolicy(cfg: NetConfig): Policy {
  return policy({
    allowHosts: new Set(cfg.allowHosts),
    denyHosts: new Set(cfg.denyHosts),
    hostMiss: cfg.hostMiss,
    k8sHosts: new Set(cfg.k8sHosts),
    mode: cfg.mode,
    allowVerbs: new Set(cfg.allowVerbs),
    denyVerbs: new Set(cfg.denyVerbs),
    holdVerbs: new Set(cfg.holdVerbs),
  });
}
