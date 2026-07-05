/**
 * Bundle install — the `bough net add <bundle>` backend. Takes a bundle + the params
 * the operator filled in, and:
 *   1. resolves + type-checks params against the manifest (required, defaults),
 *   2. renders the bundle's NetConfig contribution (hosts + verb rules),
 *   3. validates offline by running the bundle's fixtures through policy.ts (the same
 *      brain the gate enforces) against a read-only policy of the contributed hosts,
 *   4. merges the contribution into the persisted rule set (config.ts) and records the
 *      bundle as installed.
 *
 * Fully native: no HCL, no external binary. The gate hot-swaps from the merged config
 * (the server handler calls setPolicy after install).
 */
import { join } from "node:path";
import { homedir } from "node:os";
import { mkdirSync } from "node:fs";
import { compileRule, decide, policy as makePolicy, type Request, type Rule } from "./policy.ts";
import { loadConfig, type NetConfig, saveConfig } from "./config.ts";
import type { BundleContribution, BundleFixture, BundleManifest, BundleParam } from "./bundles.ts";

export interface FixtureResult {
  name: string;
  ok: boolean;
  expected: string;
  got: string;
}

export interface InstallResult {
  name: string;
  params: Record<string, unknown>;
  contribution: BundleContribution;
  fixtures: FixtureResult[];
  ok: boolean;
}

/** Thrown on bad params or failing fixtures; carries a machine-usable code. */
export class InstallError extends Error {
  constructor(message: string, readonly detail?: unknown) {
    super(message);
    this.name = "InstallError";
  }
}

/** The net config dir. BOUGH_NET_DIR overrides (tests); else ~/.bough/net. */
export function netDir(): string {
  const override = Deno.env.get("BOUGH_NET_DIR");
  const dir = override ?? join(homedir(), ".bough", "net");
  mkdirSync(dir, { recursive: true });
  return dir;
}

/** True if a bundle has been merged into the persisted rule set. */
export function isInstalled(name: string, dir = netDir()): boolean {
  return loadConfig(dir).bundles.includes(name);
}

// ---- param resolution ------------------------------------------------------

function resolveParam(p: BundleParam, raw: Record<string, unknown>): unknown {
  const has = Object.prototype.hasOwnProperty.call(raw, p.name);
  const value = has ? raw[p.name] : p.default;
  if (value === undefined) {
    if (p.required) throw new InstallError(`missing required param: ${p.name}`);
    return undefined;
  }
  const bad = (want: string) => new InstallError(`param ${p.name} must be ${want}`);
  switch (p.type) {
    case "string":
    case "host":
      if (typeof value !== "string") throw bad("a string");
      return value;
    case "hostList":
      if (!Array.isArray(value) || value.some((v) => typeof v !== "string")) {
        throw bad("a string[]");
      }
      return value;
    case "bool":
      if (typeof value !== "boolean") throw bad("a boolean");
      return value;
  }
}

function resolveParams(
  manifest: BundleManifest,
  raw: Record<string, unknown>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const p of manifest.params) {
    const v = resolveParam(p, raw);
    if (v !== undefined) out[p.name] = v;
  }
  return out;
}

// ---- offline fixture validation (policy.ts) --------------------------------

function fixtureRequest(f: BundleFixture): Request {
  const http = (f.action.http ?? {}) as {
    method?: string;
    path?: string;
    headers?: Record<string, string>;
    body?: string;
  };
  return {
    host: String(f.action.host ?? ""),
    method: http.method ?? "GET",
    path: http.path ?? "/",
    headers: http.headers ?? {},
    body: http.body,
  };
}

/**
 * Run the bundle's fixtures against a read-only policy of its verb rules. The host layer
 * is left open (empty allowHosts = sniff-only) on purpose: fixtures validate the ACTION
 * gating (read/write/hold classification + verb overrides), not host-allowlist membership
 * — which lets a bundle installed for a custom host still validate its fixtures.
 */
function runFixtures(manifest: BundleManifest, contribution: BundleContribution): FixtureResult[] {
  const pol = makePolicy({
    k8sHosts: new Set(contribution.k8sHosts ?? []),
    mode: "read_only",
    rules: (contribution.rules ?? []).map(compileRule),
    allowVerbs: new Set(contribution.allowVerbs ?? []),
    denyVerbs: new Set(contribution.denyVerbs ?? []),
    holdVerbs: new Set(contribution.holdVerbs ?? []),
  });
  return manifest.fixtures.map((f) => {
    const got = decide(fixtureRequest(f), pol);
    const ruleOk = f.expect.rule === undefined || got.rule === f.expect.rule;
    return {
      name: f.name,
      ok: got.verdict === f.expect.verdict && ruleOk,
      expected: f.expect.rule ? `${f.expect.verdict} (rule ${f.expect.rule})` : f.expect.verdict,
      got: got.rule ? `${got.verdict} (rule ${got.rule})` : got.verdict,
    };
  });
}

// ---- compose + validate + merge --------------------------------------------

/** Resolve params, render the contribution, and run fixtures — no disk writes. */
export function validateInstall(
  manifest: BundleManifest,
  rawParams: Record<string, unknown> = {},
): InstallResult {
  const params = resolveParams(manifest, rawParams);
  const contribution = manifest.render(params);
  const fixtures = runFixtures(manifest, contribution);
  return { name: manifest.name, params, contribution, fixtures, ok: fixtures.every((f) => f.ok) };
}

/** Union two string lists, order-stable, deduped. */
function union(a: string[], b: string[] = []): string[] {
  return [...new Set([...a, ...b])];
}

/** Merge rules by name: a re-installed bundle's rule replaces its previous version in place. */
function mergeRules(existing: NetConfig["rules"], contributed: Rule[] = []): NetConfig["rules"] {
  const incoming = new Map(contributed.map((r) => [r.name, r]));
  const merged = existing.map((r) => incoming.get(r.name) ?? r);
  const known = new Set(existing.map((r) => r.name));
  merged.push(...contributed.filter((r) => !known.has(r.name)));
  return merged;
}

/** Merge a bundle's contribution into the persisted rule set + record it as installed. */
function mergeIntoConfig(name: string, c: BundleContribution, dir: string): NetConfig {
  const cfg = loadConfig(dir);
  return saveConfig({
    ...cfg,
    allowHosts: union(cfg.allowHosts, c.allowHosts),
    denyHosts: union(cfg.denyHosts, c.denyHosts),
    k8sHosts: union(cfg.k8sHosts, c.k8sHosts),
    rules: mergeRules(cfg.rules, c.rules),
    allowVerbs: union(cfg.allowVerbs, c.allowVerbs),
    denyVerbs: union(cfg.denyVerbs, c.denyVerbs),
    holdVerbs: union(cfg.holdVerbs, c.holdVerbs),
    bundles: union(cfg.bundles, [name]),
  }, dir);
}

/** Validate then merge the bundle into the live rule set. Throws InstallError if fixtures fail. */
export function installBundle(
  manifest: BundleManifest,
  rawParams: Record<string, unknown> = {},
  dir = netDir(),
): InstallResult {
  const result = validateInstall(manifest, rawParams);
  if (!result.ok) {
    throw new InstallError("bundle fixtures failed validation", result.fixtures);
  }
  mergeIntoConfig(manifest.name, result.contribution, dir);
  return result;
}
