/**
 * Classifier plugins — how the operator (or a frontier model) teaches the gate a
 * provider's verb vocabulary, so destructive operations can be denied/held per-op
 * instead of per-host. A plugin is ONE readable TS module in ~/.bough/net/plugins/:
 *
 *   export const meta = { name, description, hosts, expires? };  // expires = ISO TTL
 *   export const ops = [ { match: "POST /v1/refunds*", kind: "write", verb: "stripe:refund" } ];
 *   export function classify(req) { ... }   // escape hatch; optional when ops exists
 *   export const fixtures = [ { req, expect } ];  // REQUIRED — run before it gates
 *
 * The declarative `ops` table is the primary form: first match wins, the UI renders
 * it as a table, and the LLM draft path (suggest.ts) emits it as pure JSON — no code
 * execution at draft time. `classify()` exists for what tables can't say (e.g.
 * tokenizing GraphQL); when both exist, classify() runs first and undefined falls to
 * the table.
 *
 * `gate()` is the CONTEXTUAL layer — for verdicts the request's shape can't decide
 * ("delete is fine only if the resource is <1h old"). It runs in the gate path after
 * the static decision, only for requests to this plugin's hosts, and may be async:
 * ctx.fetch egresses DIRECTLY from the server process (no proxy → no gate recursion,
 * and it may use standing that the sandbox never gets), so it can make an
 * out-of-band check request — e.g. HEAD the object and read Last-Modified — while
 * the gated request sits parked. Returning {verdict, reason} overrides the static
 * decision; undefined passes. A throw or timeout falls through — the static posture
 * still gates, so a broken gate() can never open the firewall.
 *
 * Plugin FILES are a global library; they gate nothing by themselves. A plugin is
 * turned ON per scope by an ACTIVATION entry in the rule set (NetConfig.plugins —
 * global policy.json or a branch's override row, inherited down the session tree),
 * and the TTL lives on the activation, so one library plugin can run open-ended in
 * one branch and lapse after 2h in another. `activeFor()` is the runtime join.
 *
 * Fail-closed invariants (the whole point):
 *   - a plugin owns its hosts: while its activation is live they skip the allowHosts
 *     gate (enabling IS the trust decision — see decide()), and an ops table with no
 *     matching row classifies UNKNOWN, which read_only denies and review holds;
 *   - a plugin that fails to load (bad meta, failing fixtures, name collision) is
 *     listed with its error but NEVER gates — its hosts fall back to the built-in
 *     chain, where unrecognised traffic is UNKNOWN;
 *   - an expired or missing activation drops the classifier lazily on the next
 *     request — expiry can only REMOVE precision/permissiveness, never add it;
 *   - a classify() that throws yields UNKNOWN for that request, not fall-through.
 *
 * TRUST MODEL: plugin files are the operator's own code and
 * run in the server process. The declarative draft/install path never executes model
 * output; it validates the JSON spec, runs its fixtures in-memory, and renders the
 * module file only when the operator installs it.
 */
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { existsSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { z } from "zod/v4";
import {
  type Action,
  type Classifier,
  type Decision,
  hostMatches,
  type Kind,
  type Request,
  UNKNOWN,
  type Verdict,
} from "./policy.ts";
import type { PluginActivation } from "./config.ts";
import { netDir } from "./install.ts";

const KindSchema = z.enum(["read", "write", "unknown"]);

/** One row of the declarative classifier table. `match` is "METHOD /path-glob" ("*" wildcards). */
export const OpRule = z.object({
  match: z.string().regex(
    /^\S+ \S.*$/,
    'match must be "METHOD /path-glob", e.g. "POST /v1/refunds*"',
  ),
  kind: KindSchema,
  /** Rule-set verb this op classifies as; default "<METHOD> <path>" of the actual request. */
  verb: z.string().optional(),
});
export type OpRule = z.infer<typeof OpRule>;

export const PluginMeta = z.object({
  name: z.string().regex(/^[a-z0-9][a-z0-9-]*$/, "name must be a lowercase slug"),
  description: z.string().optional(),
  /** Hosts this plugin classifies (exact or "*.suffix"). It owns these entirely. */
  hosts: z.array(z.string().min(1)).min(1),
});
export type PluginMeta = z.infer<typeof PluginMeta>;

export const PluginFixture = z.object({
  name: z.string().optional(),
  req: z.object({
    method: z.string().min(1),
    path: z.string().min(1),
    host: z.string().optional(),
    body: z.string().optional(),
  }),
  expect: z.object({ kind: KindSchema.optional(), verb: z.string().optional() })
    .refine((e) => e.kind !== undefined || e.verb !== undefined, "expect kind and/or verb"),
});
export type PluginFixture = z.infer<typeof PluginFixture>;

/** The declarative plugin — what the LLM drafts and the install endpoint accepts. */
export const PluginSpec = z.object({
  meta: PluginMeta,
  ops: z.array(OpRule).min(1),
  fixtures: z.array(PluginFixture).min(1),
});
export type PluginSpec = z.infer<typeof PluginSpec>;

/** What a plugin's gate() sees alongside the raw request. */
export interface GuardCtx {
  sessionId?: string;
  /** The classified action ("s3:delete", "GET /x", …). */
  action: Action;
  /** What the static rule set decided; gate() may override it. */
  decision: Decision;
  /**
   * Plain fetch, running in the SERVER process: egresses directly (no proxy env, so
   * no gate recursion) for out-of-band checks like HEADing a resource for its age.
   */
  fetch: typeof fetch;
}

export type GuardResult = { verdict: Verdict; reason?: string } | undefined;
export type GuardFn = (req: Request, ctx: GuardCtx) => GuardResult | Promise<GuardResult>;

/** A live gate() the Gate consults for requests to the plugin's hosts. */
export interface PluginGuard {
  name: string;
  hosts: string[];
  gate: GuardFn;
}

/** What GET /net/plugins lists — the library; activation state lives in the rule set. */
export interface PluginInfo {
  name: string;
  file: string;
  description?: string;
  hosts: string[];
  ops?: OpRule[];
  /** True when the module exports a classify() beyond (or instead of) the ops table. */
  hasClassify: boolean;
  /** True when the module exports a contextual gate(). */
  hasGate: boolean;
  fixtures: number;
  status: "loaded" | "error";
  error?: string;
}

interface Loaded {
  meta: PluginMeta;
  file: string;
  ops?: OpRule[];
  hasClassify: boolean;
  fixtures: number;
  classifier: Classifier;
  gate?: GuardFn;
}

/** The plugins dir: <netDir>/plugins. */
export function pluginsDir(dir = netDir()): string {
  return join(dir, "plugins");
}

// ---- ops matcher -------------------------------------------------------------

interface CompiledOp {
  method: string; // uppercase, or "*"
  path: RegExp;
  kind: Kind;
  verb?: string;
}

function compileOps(ops: OpRule[]): CompiledOp[] {
  return ops.map((op) => {
    const space = op.match.indexOf(" ");
    const method = op.match.slice(0, space).toUpperCase();
    const glob = op.match.slice(space + 1);
    const pattern = glob.split("*").map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join(
      ".*",
    );
    return { method, path: new RegExp(`^${pattern}$`), kind: op.kind as Kind, verb: op.verb };
  });
}

/**
 * Build the Classifier for a plugin: custom classify() first (throw ⇒ UNKNOWN, not
 * fall-through), then the ops table (first match wins; no match ⇒ UNKNOWN — the
 * plugin owns its hosts). A classify()-only plugin returning undefined falls through
 * to the built-in chain, mirroring classifyGraphql.
 */
export function buildClassifier(
  name: string,
  hosts: string[],
  ops?: OpRule[],
  custom?: (req: Request) => Action | undefined,
): Classifier {
  const compiled = ops ? compileOps(ops) : undefined;
  return {
    name,
    hosts,
    classify(req: Request): Action | undefined {
      const method = req.method.toUpperCase();
      const path = req.path.split("?")[0];
      if (custom) {
        try {
          const out = custom(req);
          if (out) return out;
        } catch (e) {
          console.error(`[clawpatrol] plugin ${name} classify() threw: ${(e as Error).message}`);
          return { service: name, verb: `${method} ${path}`, kind: UNKNOWN };
        }
      }
      if (!compiled) return undefined;
      for (const op of compiled) {
        if (op.method !== "*" && op.method !== method) continue;
        if (!op.path.test(path)) continue;
        return { service: name, verb: op.verb ?? `${method} ${path}`, kind: op.kind };
      }
      return { service: name, verb: `${method} ${path}`, kind: UNKNOWN };
    },
  };
}

// ---- fixtures ----------------------------------------------------------------

/**
 * Run a plugin's fixtures against its classifier. Pure and synchronous — the same
 * check gates loading a file AND accepting an LLM draft, so a wrong table never
 * reaches the wire either way.
 */
export function runFixtures(
  classifier: Classifier,
  hosts: string[],
  fixtures: PluginFixture[],
): string[] {
  const failures: string[] = [];
  fixtures.forEach((f, i) => {
    const label = f.name ?? `#${i + 1} ${f.req.method} ${f.req.path}`;
    const req: Request = {
      host: f.req.host ?? hosts[0].replace(/^\*\./, ""),
      method: f.req.method,
      path: f.req.path,
      body: f.req.body,
    };
    const action = classifier.classify(req);
    if (!action) {
      failures.push(`${label}: classified nothing (fell through)`);
      return;
    }
    if (f.expect.kind !== undefined && action.kind !== f.expect.kind) {
      failures.push(`${label}: kind ${action.kind}, expected ${f.expect.kind}`);
    }
    if (f.expect.verb !== undefined && action.verb !== f.expect.verb) {
      failures.push(`${label}: verb "${action.verb}", expected "${f.expect.verb}"`);
    }
  });
  return failures;
}

// ---- rendering ---------------------------------------------------------------

/** TTL shorthand ("90m", "2h", "7d") → ISO expiry from now. */
export function ttlToExpires(ttl: string, now = Date.now()): string {
  const m = ttl.trim().match(/^(\d+)\s*(m|h|d)$/);
  if (!m) throw new Error(`invalid ttl "${ttl}" — use e.g. "90m", "2h", "7d"`);
  const unit = { m: 60_000, h: 3_600_000, d: 86_400_000 }[m[2] as "m" | "h" | "d"];
  return new Date(now + Number(m[1]) * unit).toISOString();
}

function opLine(op: OpRule): string {
  const parts = [`match: ${JSON.stringify(op.match)}`, `kind: ${JSON.stringify(op.kind)}`];
  if (op.verb !== undefined) parts.push(`verb: ${JSON.stringify(op.verb)}`);
  return `  { ${parts.join(", ")} },`;
}

/** Render a declarative spec as the readable module file the loader consumes. */
export function renderModule(spec: PluginSpec): string {
  return `/**
 * ${spec.meta.name} — a Claw Patrol classifier plugin.
 * Maps ${spec.meta.hosts.join(", ")} traffic onto verbs the rule set can gate.
 * Edit freely (first matching op wins; no match fails closed as "unknown"),
 * then hit Reload. Fixtures below re-run on every load.
 */
export const meta = ${JSON.stringify(spec.meta, null, 2)};

export const ops = [
${spec.ops.map(opLine).join("\n")}
];

export const fixtures = ${JSON.stringify(spec.fixtures, null, 2)};
`;
}

/** A runnable starter plugin, ready to edit. Gates nothing until hosts are real. */
export function scaffoldSpec(name: string): PluginSpec {
  return {
    meta: {
      name,
      description: `Classify api.example.com traffic (edit me).`,
      hosts: ["api.example.com"],
    },
    ops: [
      { match: "GET *", kind: "read" },
      { match: "DELETE *", kind: "write", verb: `${name}:delete` },
    ],
    fixtures: [
      { req: { method: "GET", path: "/v1/things" }, expect: { kind: "read" } },
      { req: { method: "DELETE", path: "/v1/things/42" }, expect: { verb: `${name}:delete` } },
    ],
  };
}

// ---- synthesize from observed traffic ------------------------------------------

/** One feed row's worth of what we need to build an op from. */
export interface RequestSample {
  host: string;
  verb?: string;
  action: string;
}

/** A plugin name from a host: "api.stripe.com" → "stripe", "sts.us-east-2.amazonaws.com" → "amazonaws". */
function nameFromHost(host: string): string {
  const parts = host.toLowerCase().replace(/^api\./, "").split(".");
  // second-level label (skip the TLD): stripe.com→stripe, foo.eks.amazonaws.com→amazonaws
  const label = parts.length >= 2 ? parts[parts.length - 2] : parts[0];
  return slug(label);
}

/**
 * Build a classifier plugin from selected feed requests — the "group into plugin"
 * path. Deterministic (no model): each distinct action becomes an op row, keyed by
 * the request's method + path when the action is "METHOD /path", else a method
 * catch-all. GET/HEAD classify read, everything else write (edit the rendered file
 * to name destructive verbs / generalize globs). Fixtures echo the real requests so
 * the plugin validates on load. `hosts` covers every distinct host in the selection.
 */
export function specFromRequests(samples: RequestSample[]): PluginSpec {
  if (samples.length === 0) throw new Error("no requests to build a plugin from");
  const hosts = [...new Set(samples.map((s) => s.host.toLowerCase()))];
  const ops = new Map<string, OpRule>();
  const fixtures = new Map<string, PluginFixture>();

  for (const s of samples) {
    const m = /^([A-Za-z]+)\s+(\/\S*)$/.exec(s.action);
    const method = (m?.[1] ?? s.verb ?? "GET").toUpperCase();
    const path = m?.[2] ?? "*";
    const kind: OpRule["kind"] = method === "GET" || method === "HEAD" ? "read" : "write";
    const match = `${method} ${path}`;
    if (ops.has(match)) continue;
    ops.set(match, { match, kind });
    // A fixture path that actually satisfies the glob (turn any "*" into a literal).
    const fixturePath = path === "*" ? "/probe" : path.replace(/\*/g, "x");
    fixtures.set(match, { req: { method, path: fixturePath }, expect: { kind } });
  }

  return {
    meta: {
      name: nameFromHost(hosts[0]),
      description: `Generated from ${samples.length} request(s) to ${hosts.join(", ")}.`,
      hosts,
    },
    ops: [...ops.values()],
    fixtures: [...fixtures.values()],
  };
}

// ---- host --------------------------------------------------------------------

/** Slugify a display name into a safe filename stem / plugin name. */
function slug(name: string): string {
  return name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "plugin";
}

export class PluginHost {
  #loaded: Loaded[] = [];
  #errors: PluginInfo[] = [];
  #dir: string;

  constructor(dir = pluginsDir()) {
    this.#dir = dir;
  }

  get dir(): string {
    return this.#dir;
  }

  /** The loaded + broken library for GET /net/plugins. */
  list(): PluginInfo[] {
    return [
      ...this.#loaded.map((p): PluginInfo => ({
        name: p.meta.name,
        file: p.file,
        description: p.meta.description,
        hosts: p.meta.hosts,
        ops: p.ops,
        hasClassify: p.hasClassify,
        hasGate: p.gate !== undefined,
        fixtures: p.fixtures,
        status: "loaded",
      })),
      ...this.#errors,
    ];
  }

  /**
   * The classifiers a scope's activations turn on NOW — the runtime join between the
   * library and NetConfig.plugins. Per-activation TTL is enforced here, lazily; an
   * activation naming a plugin that isn't loaded (deleted file, load error) is
   * silently inert, which fails closed at the host layer.
   */
  activeFor(activations: readonly PluginActivation[], now = Date.now()): Classifier[] {
    const out: Classifier[] = [];
    for (const a of activations) {
      if (a.expires !== undefined && Date.parse(a.expires) <= now) continue;
      const p = this.#loaded.find((l) => l.meta.name === a.name);
      if (p) out.push(p.classifier);
    }
    return out;
  }

  /** The contextual gate() hooks the same activations turn on (subset with a gate). */
  activeGuardsFor(activations: readonly PluginActivation[], now = Date.now()): PluginGuard[] {
    const out: PluginGuard[] = [];
    for (const a of activations) {
      if (a.expires !== undefined && Date.parse(a.expires) <= now) continue;
      const p = this.#loaded.find((l) => l.meta.name === a.name);
      if (p?.gate) out.push({ name: p.meta.name, hosts: p.meta.hosts, gate: p.gate });
    }
    return out;
  }

  /**
   * (Re)load every *.ts/*.js module in the dir. Broken files (bad shape, failing
   * fixtures, duplicate names) are recorded, never gate, and never abort the rest.
   */
  async load(): Promise<void> {
    this.#loaded = [];
    this.#errors = [];
    if (!existsSync(this.#dir)) return;
    const files = readdirSync(this.#dir).filter((f) => /\.(ts|js|mts|mjs)$/.test(f)).sort();
    for (const f of files) {
      const file = join(this.#dir, f);
      try {
        const mod = await import(`${pathToFileURL(file).href}?v=${Date.now()}`);
        this.#loaded.push(this.#validate(mod, file));
      } catch (e) {
        this.#errors.push({
          name: f,
          file,
          hosts: [],
          hasClassify: false,
          hasGate: false,
          fixtures: 0,
          status: "error",
          error: (e as Error).message,
        });
        console.error(`[clawpatrol] plugin ${f} failed to load: ${(e as Error).message}`);
      }
    }
    if (this.#loaded.length) {
      console.log(
        `[clawpatrol] ${this.#loaded.length} plugin(s): ${
          this.#loaded.map((p) => p.meta.name).join(", ")
        }`,
      );
    }
  }

  /** Write a starter plugin file and (re)load. Refuses to clobber. */
  async scaffold(name: string): Promise<{ path: string }> {
    const stem = slug(name);
    const path = this.#freshPath(stem);
    mkdirSync(this.#dir, { recursive: true });
    writeFileSync(path, renderModule(scaffoldSpec(stem)));
    await this.load();
    return { path };
  }

  /**
   * Install a declarative spec into the library (the /net-plugin skill's path):
   * validate, run fixtures in-memory, render the module file, reload. Throws before
   * touching disk if the spec is malformed or its fixtures fail. Activation (and its
   * TTL) is a separate, per-scope step — see config.ts setPluginActivation.
   */
  async install(
    raw: unknown,
    opts: { uniqueName?: boolean } = {},
  ): Promise<{ path: string; name: string }> {
    const spec = PluginSpec.parse(raw);
    // "Group into plugin" re-runs would collide on the host-derived name; dedupe it
    // (stripe, stripe-2, …) instead of erroring, so the button is idempotent-ish.
    if (opts.uniqueName) spec.meta.name = this.freshName(spec.meta.name);
    const failures = runFixtures(
      buildClassifier(spec.meta.name, spec.meta.hosts, spec.ops),
      spec.meta.hosts,
      spec.fixtures,
    );
    if (failures.length) throw new Error(`fixtures failed: ${failures.join("; ")}`);
    const path = this.#freshPath(spec.meta.name);
    mkdirSync(this.#dir, { recursive: true });
    writeFileSync(path, renderModule(spec));
    await this.load();
    return { path, name: spec.meta.name };
  }

  /** A plugin name whose <name>.ts file doesn't exist yet (base, base-2, base-3, …). */
  freshName(base: string): string {
    if (!existsSync(join(this.#dir, `${base}.ts`))) return base;
    for (let n = 2;; n++) {
      const cand = `${base}-${n}`;
      if (!existsSync(join(this.#dir, `${cand}.ts`))) return cand;
    }
  }

  #freshPath(stem: string): string {
    const path = join(this.#dir, `${stem}.ts`);
    if (existsSync(path)) throw new Error(`a plugin file already exists at ${path}`);
    return path;
  }

  /** Validate one imported module into a Loaded record. Throws with a clear message. */
  #validate(mod: Record<string, unknown>, file: string): Loaded {
    const meta = PluginMeta.parse(mod.meta ?? {});
    if (this.#loaded.some((p) => p.meta.name === meta.name)) {
      throw new Error(`plugin name "${meta.name}" is already registered by another file`);
    }
    const ops = mod.ops === undefined ? undefined : z.array(OpRule).min(1).parse(mod.ops);
    const custom = mod.classify as ((req: Request) => Action | undefined) | undefined;
    if (custom !== undefined && typeof custom !== "function") {
      throw new Error("classify must be a function");
    }
    const gateFn = mod.gate as GuardFn | undefined;
    if (gateFn !== undefined && typeof gateFn !== "function") {
      throw new Error("gate must be a function");
    }
    if (!ops && !custom) throw new Error("plugin must export an ops table and/or classify()");
    if (!Array.isArray(mod.fixtures) || mod.fixtures.length === 0) {
      throw new Error("plugin must export at least one fixture — fixtures gate loading");
    }
    const fixtures = z.array(PluginFixture).parse(mod.fixtures);
    const classifier = buildClassifier(meta.name, meta.hosts, ops, custom);
    const failures = runFixtures(classifier, meta.hosts, fixtures);
    if (failures.length) throw new Error(`fixtures failed: ${failures.join("; ")}`);
    return {
      meta,
      file,
      ops,
      hasClassify: custom !== undefined,
      fixtures: fixtures.length,
      classifier,
      gate: gateFn,
    };
  }
}

// ---- contextual guards ---------------------------------------------------------

const GUARD_TIMEOUT_MS = 10_000;

export interface RunGuardOpts {
  /** Guard timeout override (tests). */
  timeoutMs?: number;
  /** fetch handed to guards via ctx.fetch (tests stub upstream APIs with this). */
  fetchImpl?: typeof fetch;
}

/**
 * Run the gate() hooks whose plugin claims this request's host; the FIRST verdict
 * wins and overrides the static decision. undefined from everyone = the static rule
 * set stands. A throw or timeout logs and falls through — a broken or hanging gate()
 * can never open the firewall, only leave the static posture in charge.
 */
export async function runGuards(
  guards: readonly PluginGuard[],
  req: Request,
  decision: Decision,
  sessionId?: string,
  opts: RunGuardOpts = {},
): Promise<{ verdict: Verdict; reason: string; by: string } | undefined> {
  const host = req.host.toLowerCase();
  for (const g of guards) {
    if (!hostMatches(host, g.hosts)) continue;
    const ctx: GuardCtx = {
      sessionId,
      action: decision.action,
      decision,
      fetch: opts.fetchImpl ?? globalThis.fetch,
    };
    try {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const out = await Promise.race([
        Promise.resolve(g.gate(req, ctx)),
        new Promise<never>((_, reject) => {
          timer = setTimeout(
            () => reject(new Error("gate timed out")),
            opts.timeoutMs ?? GUARD_TIMEOUT_MS,
          );
        }),
      ]).finally(() => clearTimeout(timer));
      if (out === undefined) continue;
      return {
        verdict: out.verdict,
        reason: out.reason ?? `plugin ${g.name} gate: ${out.verdict}`,
        by: g.name,
      };
    } catch (e) {
      console.error(`[clawpatrol] plugin ${g.name} gate() errored: ${(e as Error).message}`);
    }
  }
  return undefined;
}
