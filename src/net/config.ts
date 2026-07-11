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
import { compileRule, type Policy, policy } from "./policy.ts";
import { compile } from "./expr.ts";
import { netDir } from "./install.ts";
import type { Db } from "../db/db.ts";

const Verdict = z.enum(["allow", "deny", "hold"]);

/**
 * One condition rule (see policy.ts Rule). The zod parse COMPILES the condition, so
 * a malformed expression is rejected at edit time (PUT /net/policy → 400) and a
 * corrupt persisted rule makes loadConfig fall back to the default posture — a bad
 * rule never reaches the gate half-working.
 */
export const RuleConfig = z.object({
  name: z.string().min(1),
  hosts: z.array(z.string().min(1)).optional(),
  condition: z.string().min(1).superRefine((src, ctx) => {
    try {
      compile(src);
    } catch (e) {
      ctx.addIssue(`condition does not compile: ${(e as Error).message}`);
    }
  }),
  verdict: Verdict.optional(),
  /** Ordered approver chain — every entry must allow. Mutually exclusive with verdict. */
  approve: z.array(z.string().regex(/^(human|plugin:[a-z0-9][a-z0-9-]*)$/)).optional(),
  reason: z.string().optional(),
}).superRefine((r, ctx) => {
  const hasVerdict = r.verdict !== undefined;
  const hasApprove = (r.approve?.length ?? 0) > 0;
  if (hasVerdict === hasApprove) {
    ctx.addIssue("a rule needs exactly one of verdict or approve");
  }
});
export type RuleConfig = z.infer<typeof RuleConfig>;

export const NetConfig = z.object({
  /**
   * Baseline action gate for allowed hosts: read_only (writes deny) | review (writes
   * hold) | all — or "yolo": enforcement off, log-only with shadow verdicts (see
   * policy.ts decide). Toggle yolo per scope with setYolo (the UI's red button).
   */
  mode: z.enum(["read_only", "review", "all", "yolo"]).default("review"),
  /** Set while mode is "yolo": the mode the toggle restores when flipped off. */
  prevMode: z.enum(["read_only", "review", "all"]).optional(),
  /** Trusted hosts. A request to a host NOT here gets `hostMiss`. Empty = every host allowed (sniff-only). */
  allowHosts: z.array(z.string()).default([]),
  /** Hosts denied outright (win over allowHosts). */
  denyHosts: z.array(z.string()).default([]),
  /** What to do with a host that misses a non-empty allowHosts: fail closed (deny), ask (hold), or allow. */
  hostMiss: Verdict.default("hold"),
  /** API-server hosts classified as kubernetes (verb = HTTP method). */
  k8sHosts: z.array(z.string()).default([]),
  /**
   * Condition rules — ordered, first match wins, evaluated before the verb lists and
   * mode baseline. The sharp layer: conditions (expr.ts) over facet fields, verdicts
   * or approver chains. Unevaluable conditions fail closed in decide().
   */
  rules: z.array(RuleConfig).default([]),
  /** Explicit per-action overrides by classified verb (e.g. "DELETE /repos/o/r", "graphql:mutation"). */
  allowVerbs: z.array(z.string()).default([]),
  denyVerbs: z.array(z.string()).default([]),
  holdVerbs: z.array(z.string()).default([]),
  /** Names of policy bundles merged into this config (metadata; drives `installed` in the UI). */
  bundles: z.array(z.string()).default([]),
  /**
   * Credential injections contributed by installed bundles: stamp `header` on requests
   * to `host` with the value from bough's env var `env` (only the NAME is stored — the
   * secret stays in bough's environment). The proxy resolves these host-side at request
   * time (credentials.ts); the sandbox never sees the token.
   */
  credentials: z.array(z.object({
    host: z.string(),
    header: z.string(),
    env: z.string(),
    template: z.string().optional(),
  })).default([]),
  /**
   * Classifier-plugin activations. Plugin FILES (~/.bough/net/plugins/) are a global
   * library; a plugin only gates where an activation names it — globally here, or per
   * branch via a session's net_policies override (inherited down the session tree
   * like every other field). `expires` is per-activation, so the same plugin can run
   * open-ended in one branch and lapse after 2h in another. An expired or missing
   * activation drops the classifier and the host fails closed again.
   */
  plugins: z.array(z.object({ name: z.string(), expires: z.string().optional() })).default([]),
});
export type NetConfig = z.infer<typeof NetConfig>;
export type PluginActivation = NetConfig["plugins"][number];

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

/** Where a session's effective config came from (the rule editor shows this). */
export interface PolicySource {
  scope: "session" | "inherited" | "global";
  /** For "session"/"inherited": the session whose net_policies row supplied the config. */
  sessionId?: string;
}

/**
 * The effective config for a session: its own net_policies row, else the nearest
 * ancestor's (sessions form a tree — forks and subagents inherit their branch's
 * rules), else the global policy.json. A corrupt row is skipped, not fatal.
 */
export function resolveConfig(
  db: Db,
  sessionId: string | undefined,
  dir = netDir(),
): { config: NetConfig; source: PolicySource } {
  if (sessionId) {
    const chain = db.ancestorChain(sessionId); // root first; walk self → root
    for (let i = chain.length - 1; i >= 0; i--) {
      const raw = db.getNetPolicy(chain[i].id);
      if (raw === undefined) continue;
      try {
        const config = NetConfig.parse(JSON.parse(raw));
        const scope = chain[i].id === sessionId ? "session" as const : "inherited" as const;
        return { config, source: { scope, sessionId: chain[i].id } };
      } catch {
        // corrupt override — ignore it and keep walking up
      }
    }
  }
  return { config: loadConfig(dir), source: { scope: "global" } };
}

/**
 * Turn a plugin activation on (upsert; `expires` undefined = open-ended) or off for
 * one scope. Global scope edits policy.json; session scope writes the branch's
 * net_policies override seeded from its EFFECTIVE config — the same copy-on-write
 * move as the rule editor's override flow, so children keep inheriting it.
 */
export function setPluginActivation(
  db: Db,
  sessionId: string | undefined,
  name: string,
  on: boolean,
  expires?: string,
  dir = netDir(),
): NetConfig {
  const config = sessionId ? resolveConfig(db, sessionId, dir).config : loadConfig(dir);
  const plugins = config.plugins.filter((p) => p.name !== name);
  if (on) plugins.push({ name, ...(expires ? { expires } : {}) });
  const next = { ...config, plugins };
  if (sessionId) db.setNetPolicy(sessionId, JSON.stringify(next));
  else saveConfig(next, dir);
  return next;
}

/**
 * Flip YOLO (log-only, no gating) on/off for one scope — the red button's backend.
 * Same copy-on-write move as setPluginActivation: session scope writes the branch's
 * net_policies override seeded from its EFFECTIVE config (children inherit it),
 * global scope edits policy.json. The pre-yolo mode rides along as `prevMode` so
 * flipping off restores exactly what the scope ran before. Idempotent both ways.
 */
export function setYolo(
  db: Db,
  sessionId: string | undefined,
  on: boolean,
  dir = netDir(),
): NetConfig {
  const config = sessionId ? resolveConfig(db, sessionId, dir).config : loadConfig(dir);
  let next: NetConfig;
  if (on) {
    if (config.mode === "yolo") return config;
    next = { ...config, mode: "yolo", prevMode: config.mode };
  } else {
    if (config.mode !== "yolo") return config;
    const { prevMode: _dropped, ...rest } = config;
    next = { ...rest, mode: config.prevMode ?? "review" };
  }
  if (sessionId) db.setNetPolicy(sessionId, JSON.stringify(next));
  else saveConfig(next, dir);
  return next;
}

/** Compile the editable config into the runtime Policy the gate enforces. */
export function toPolicy(cfg: NetConfig): Policy {
  return policy({
    allowHosts: new Set(cfg.allowHosts),
    denyHosts: new Set(cfg.denyHosts),
    hostMiss: cfg.hostMiss,
    k8sHosts: new Set(cfg.k8sHosts),
    mode: cfg.mode,
    rules: cfg.rules.map(compileRule),
    allowVerbs: new Set(cfg.allowVerbs),
    denyVerbs: new Set(cfg.denyVerbs),
    holdVerbs: new Set(cfg.holdVerbs),
  });
}
