/**
 * Bundle install — the `bough net add <bundle>` backend. Takes a bundle + the params
 * the Driver filled in on Screen 5, and:
 *   1. resolves + type-checks params against the manifest (required, defaults),
 *   2. renders the bundle's HCL fragment and composes it into a full gateway policy
 *      (hcl.ts → text the Claw Patrol gateway loads),
 *   3. validates offline by running the bundle's own fixtures through policy.ts (the
 *      runtime mirror of the HCL rules) — this is the `clawpatrol test` regression the
 *      design calls for, minus a live gateway,
 *   4. persists the rendered HCL to the net config dir (~/.bough/net/<name>.hcl).
 *
 * Fixture validation covers https-host bundles (the shipped `github`): the derived
 * policy allows the fragment's hosts and runs read-only unless the bundle exposes a
 * truthy `allowWrites` param. Bundles that gate other protocols extend the derivation.
 */
import { join } from "node:path";
import { homedir } from "node:os";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { defaultGateway, type HclPolicy, renderHcl } from "./hcl.ts";
import { clawpatrolTest } from "./clawpatrol.ts";
import { decide, policy as makePolicy, type Request } from "./policy.ts";
import type { BundleFixture, BundleManifest, BundleParam } from "./bundles.ts";

export interface FixtureResult {
  name: string;
  ok: boolean;
  expected: string;
  got: string;
}

export interface InstallResult {
  name: string;
  params: Record<string, unknown>;
  hcl: string;
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

/** True if a bundle's rendered policy is already on disk. */
export function isInstalled(name: string, dir = netDir()): boolean {
  return existsSync(join(dir, `${name}.hcl`));
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

function resolveParams(manifest: BundleManifest, raw: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const p of manifest.params) {
    const v = resolveParam(p, raw);
    if (v !== undefined) out[p.name] = v;
  }
  return out;
}

// ---- offline fixture validation (policy.ts mirror) -------------------------

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

function runFixtures(
  manifest: BundleManifest,
  params: Record<string, unknown>,
  hosts: string[],
): FixtureResult[] {
  const pol = makePolicy({
    allowHosts: new Set(hosts),
    mode: params.allowWrites ? "all" : "read_only",
  });
  return manifest.fixtures.map((f) => {
    const got = decide(fixtureRequest(f), pol).verdict;
    return { name: f.name, ok: got === f.expect.verdict, expected: f.expect.verdict, got };
  });
}

// ---- compose + validate + persist ------------------------------------------

/** Resolve params, render HCL, and run fixtures — no disk writes. */
export function validateInstall(
  manifest: BundleManifest,
  rawParams: Record<string, unknown> = {},
): InstallResult {
  const params = resolveParams(manifest, rawParams);
  const fragment = manifest.render(params);
  const hosts = fragment.endpoints.flatMap((e) => e.hosts ?? (e.host ? [e.host] : []));

  const policy: HclPolicy = {
    gateway: defaultGateway(),
    credentials: fragment.credentials,
    endpoints: fragment.endpoints,
    rules: fragment.rules,
    profiles: [{
      name: "default",
      credentials: fragment.credentials.map((c) => `${c.type}.${c.name}`),
    }],
  };
  const hcl = renderHcl(policy);
  const fixtures = runFixtures(manifest, params, hosts);
  return { name: manifest.name, params, hcl, fixtures, ok: fixtures.every((f) => f.ok) };
}

/**
 * Validate then persist the rendered HCL. Throws InstallError if fixtures fail —
 * first against the offline policy.ts mirror, then (when the binary is installed)
 * against the REAL `clawpatrol validate` + `clawpatrol test`, which is the
 * authoritative regression: the same compiler the gateway loads the policy with.
 */
export async function installBundle(
  manifest: BundleManifest,
  rawParams: Record<string, unknown> = {},
  dir = netDir(),
): Promise<InstallResult> {
  const result = validateInstall(manifest, rawParams);
  if (!result.ok) {
    throw new InstallError("bundle fixtures failed validation", result.fixtures);
  }
  const real = await clawpatrolTest(result.hcl, manifest.fixtures);
  if (real.ran && !real.ok) {
    throw new InstallError("clawpatrol rejected the rendered policy", real.output);
  }
  if (real.flaky) {
    console.warn(
      `clawpatrol verdict for ${manifest.name} was unstable across runs (upstream ` +
        "rule-order nondeterminism — see net/clawpatrol.ts); passed on retry.",
    );
  }
  writeFileSync(join(dir, `${manifest.name}.hcl`), result.hcl);
  return result;
}
