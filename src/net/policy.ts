/**
 * Request classifier + egress policy decision — the runtime brain of the net
 * gate. Kept pure and dependency-free so it unit-tests without a gateway or a
 * network.
 *
 * Scope: we only READ requests to gate them (allow/deny/hold) — we never modify
 * them, so AWS SigV4 signatures stay valid. Credential injection is a separate
 * concern (the Claw Patrol gateway). The policy is default-deny at the host layer
 * and fail-closed on unrecognised actions.
 *
 * What "action" means per provider (unchanged from policy.py):
 *   - AWS   — JSON-protocol services carry `X-Amz-Target: Service_Ver.Operation`;
 *             query-protocol services (ec2/sts/iam) carry `Action=Foo` in the
 *             body or query string. Read ops start with describe/list/get/... .
 *   - k8s   — the HTTP verb is the action (GET=read, DELETE/POST/PUT/PATCH=write);
 *             the resource is the path. The API-server hosts are passed in.
 *   - GitHub— REST is verb+path; GraphQL is one `POST /graphql` with the operation
 *             in the body, so we peek for a `mutation` (coarse — see the note).
 *
 * Delta from policy.py: the boolean allow/deny becomes a three-valued Verdict so
 * the design's "hold-and-ask" (human-approval) gate is first-class. Parity is
 * preserved: `holdVerbs` is empty by default, so with no hold config `decide`
 * yields the exact allow/deny outcomes policy.py did (see test_policy.ts).
 */

import { compile, type ExprEnv, ExprError } from "./expr.ts";

export type Verdict = "allow" | "deny" | "hold";

export const READ = "read";
export const WRITE = "write";
export const UNKNOWN = "unknown";
export type Kind = typeof READ | typeof WRITE | typeof UNKNOWN;

/** AWS operation-name prefixes that are read-only (case-insensitive). */
const AWS_READ_PREFIXES = [
  "describe",
  "list",
  "get",
  "head",
  "lookup",
  "search",
  "query",
  "scan",
  "batchget",
  "select",
  "estimate",
  "preview",
  "validate",
  "check",
  "view",
];

/** A provider-agnostic view of one outbound request, built by the gateway addon. */
export interface Request {
  host: string;
  method: string;
  path: string;
  headers?: Record<string, string>;
  body?: string | Uint8Array;
}

export interface Action {
  /** "aws:ec2", "k8s", "github", a plugin name, "other". */
  service: string;
  /** e.g. "TerminateInstances", "DELETE /api/v1/pods/x", "graphql:mutation". */
  verb: string;
  kind: Kind;
  /**
   * Facet payload — the classifier's PARSED view of the request, published into the
   * rule env under `facet.name` so conditions can match on typed fields
   * (`k8s.resource == 'secrets'`) instead of re-parsing paths. The built-in `http`
   * facet is always in the env; this is the provider-specific one.
   */
  facet?: { name: string; fields: Record<string, unknown> };
}

/**
 * A pluggable classifier — how plugins (plugins.ts) teach the gate a provider's
 * verb vocabulary. `classify` is consulted only for hosts matching `hosts`
 * (exact or "*.suffix") and must be synchronous and pure — it runs on the gate's
 * hot path. Returning undefined falls through to the next matching classifier,
 * then the built-ins.
 */
export interface Classifier {
  name: string;
  hosts: string[];
  classify(req: Request): Action | undefined;
}

export interface Decision {
  verdict: Verdict;
  reason: string;
  action: Action;
  /**
   * Set when a matched rule routes through an approver chain instead of a static
   * verdict: the ordered approvers ("human", "plugin:<name>") that must EACH allow.
   * The verdict is "hold" while the chain runs — the gate (gate.ts) executes it.
   */
  approve?: string[];
  /** Name of the rule that decided, when one matched. */
  rule?: string;
}

// ---- condition rules ---------------------------------------------------------

/**
 * One operator-authored rule: a condition (expr.ts) over the facet env, scoped to
 * hosts, with a static verdict OR an approver chain. First matching rule wins —
 * upstream Claw Patrol semantics — and rules outrank the coarser verb lists and mode
 * baseline in decide(). An unevaluable condition fails closed (deny).
 */
export interface Rule {
  name: string;
  /** Hosts the rule applies to (exact or "*.suffix"). Absent/empty = every host. */
  hosts?: string[];
  condition: string;
  verdict?: Verdict;
  /** Approver chain, e.g. ["plugin:s3-age-check", "human"]. Mutually exclusive with verdict. */
  approve?: string[];
  reason?: string;
}

export interface CompiledRule extends Rule {
  test(env: ExprEnv): boolean;
}

/** Compile a rule's condition (throws ExprError on a malformed expression). */
export function compileRule(rule: Rule): CompiledRule {
  const expr = compile(rule.condition);
  return { ...rule, test: (env) => expr.test(env) };
}

/**
 * The env a rule condition evaluates against. Always present: `http` (the built-in
 * facet — method, path, query, headers, body, body_json) and `action` (the classified
 * {service, verb, kind}). The classifier's provider facet (k8s, graphql, aws:*, a
 * plugin's) joins under its own name. `body_json` parses lazily and THROWS on
 * non-JSON, which fails the condition closed — guard with has() or a path check.
 */
export function ruleEnv(req: Request, action: Action): ExprEnv {
  const http: Record<string, unknown> = {
    method: req.method.toUpperCase(),
    path: req.path.split("?")[0],
    query: queryMap(req.path),
    headers: lowerKeys(req.headers ?? {}),
    body: bodyText(req),
    get body_json(): unknown {
      const text = bodyText(req);
      try {
        return JSON.parse(text);
      } catch {
        throw new ExprError("body is not JSON");
      }
    },
  };
  const env: ExprEnv = {
    http,
    action: { service: action.service, verb: action.verb, kind: action.kind },
  };
  if (action.facet) env[action.facet.name] = action.facet.fields;
  return env;
}

function lowerKeys(headers: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(headers)) out[k.toLowerCase()] = v;
  return out;
}

/** Query string → map of first values (multi-valued keys keep the first). */
function queryMap(path: string): Record<string, string> {
  const q = path.indexOf("?");
  if (q < 0) return {};
  const out: Record<string, string> = {};
  for (const pair of path.slice(q + 1).split("&")) {
    const eq = pair.indexOf("=");
    if (eq < 0) continue;
    try {
      const k = decodeURIComponent(pair.slice(0, eq));
      if (!(k in out)) out[k] = decodeURIComponent(pair.slice(eq + 1).replace(/\+/g, " "));
    } catch {
      // undecodable pair — skip it
    }
  }
  return out;
}

/**
 * Egress policy — the compiled runtime form the rule-set editor produces (see
 * config.ts). Host layer: `denyHosts` win outright; `allowHosts` is the allowlist
 * (empty = allow all hosts, sniff-only); a host that misses a non-empty allowlist
 * gets `hostMiss` (deny to fail closed, or hold to ask). Action layer: `mode` is the
 * baseline for allowed hosts — "read_only" permits reads and blocks writes; "review"
 * permits reads and HOLDS writes/unknown for approval; "all" permits anything.
 * `allowVerbs` / `denyVerbs` / `holdVerbs` are explicit per-action overrides
 * (deny > hold > allow).
 */
export interface Policy {
  allowHosts: Set<string>;
  denyHosts: Set<string>;
  /** Verdict when `allowHosts` is non-empty and the host isn't in it. Default "deny". */
  hostMiss: Verdict;
  k8sHosts: Set<string>;
  mode: "read_only" | "review" | "all";
  /** Condition rules — evaluated first (ordered, first match wins) for allowed hosts. */
  rules: CompiledRule[];
  allowVerbs: Set<string>;
  denyVerbs: Set<string>;
  holdVerbs: Set<string>;
}

/** Build a Policy; unset fields default to the fail-closed-lite baseline (host-open, read-only). */
export function policy(p: Partial<Policy> = {}): Policy {
  return {
    allowHosts: p.allowHosts ?? new Set(),
    denyHosts: p.denyHosts ?? new Set(),
    hostMiss: p.hostMiss ?? "deny",
    k8sHosts: p.k8sHosts ?? new Set(),
    mode: p.mode ?? "read_only",
    rules: p.rules ?? [],
    allowVerbs: p.allowVerbs ?? new Set(),
    denyVerbs: p.denyVerbs ?? new Set(),
    holdVerbs: p.holdVerbs ?? new Set(),
  };
}

function header(req: Request, name: string): string | undefined {
  const lower = name.toLowerCase();
  for (const [k, v] of Object.entries(req.headers ?? {})) {
    if (k.toLowerCase() === lower) return v;
  }
  return undefined;
}

export function bodyText(req: Request): string {
  const b = req.body;
  if (b == null) return "";
  if (typeof b === "string") return b;
  return new TextDecoder().decode(b);
}

export function hostMatches(host: string, patterns: Iterable<string>): boolean {
  for (const p of patterns) {
    if (host === p || (p.startsWith("*.") && host.endsWith(p.slice(1)))) return true;
  }
  return false;
}

/** First value of a form-encoded key, mirroring urllib's parse_qs().get(). */
function firstQueryValue(source: string, key: string): string | undefined {
  for (const pair of source.split("&")) {
    const eq = pair.indexOf("=");
    if (eq < 0) continue;
    if (decodeURIComponent(pair.slice(0, eq)) === key) {
      return decodeURIComponent(pair.slice(eq + 1).replace(/\+/g, " "));
    }
  }
  return undefined;
}

// ---- per-provider classifiers ----------------------------------------------

function awsKind(op: string): Kind {
  const lower = op.toLowerCase();
  return AWS_READ_PREFIXES.some((p) => lower.startsWith(p)) ? READ : WRITE;
}

export function classifyAws(req: Request): Action {
  const service = "aws:" + req.host.split(".")[0];
  const target = header(req, "X-Amz-Target");
  if (target) {
    const op = target.split(".").pop()!;
    return { service, verb: op, kind: awsKind(op), facet: { name: "aws", fields: { op } } };
  }
  // query protocol: Action= in the body, else in the path's query string.
  const qs = req.path.includes("?") ? req.path.split("?").slice(1).join("?") : "";
  for (const source of [bodyText(req), qs]) {
    const action = firstQueryValue(source, "Action");
    if (action) {
      return {
        service,
        verb: action,
        kind: awsKind(action),
        facet: { name: "aws", fields: { op: action } },
      };
    }
  }
  return { service, verb: "?", kind: UNKNOWN };
}

/**
 * Best-effort parse of a k8s API path into the (verb, resource, namespace, name)
 * tuple the `k8s` facet exposes to rules — same field names upstream Claw Patrol
 * uses (`k8s.resource == 'pods/exec'`). Subresources join with "/".
 */
function k8sFields(method: string, path: string): Record<string, unknown> {
  const parts = path.split("?")[0].split("/").filter(Boolean);
  let i = 0;
  if (parts[0] === "api") i = 2; // /api/v1/...
  else if (parts[0] === "apis") i = 3; // /apis/<group>/<version>/...
  let namespace = "";
  if (parts[i] === "namespaces" && parts[i + 1]) {
    namespace = parts[i + 1];
    i += 2;
  }
  const resource = parts[i] ?? "";
  const name = parts[i + 1] ?? "";
  const sub = parts.slice(i + 2).join("/");
  const verb = method === "GET"
    ? (name ? "get" : "list")
    : { POST: "create", PUT: "update", PATCH: "patch", DELETE: "delete", HEAD: "get" }[method] ??
      method.toLowerCase();
  return { verb, resource: sub ? `${resource}/${sub}` : resource, namespace, name };
}

export function classifyK8s(req: Request): Action {
  const verb = req.method.toUpperCase();
  const kind: Kind = verb === "GET" || verb === "HEAD"
    ? READ
    : ["POST", "PUT", "PATCH", "DELETE"].includes(verb)
    ? WRITE
    : UNKNOWN;
  const resource = req.path.split("?")[0];
  return {
    service: "k8s",
    verb: `${verb} ${resource}`,
    kind,
    facet: { name: "k8s", fields: k8sFields(verb, req.path) },
  };
}

/**
 * GraphQL on ANY host: a path ending in /graphql gets operation-level verbs
 * (graphql:query | graphql:mutation) by peeking at the decrypted body — the MITM
 * proxy hands us the plaintext. Detection is path-based on purpose: sniffing bodies
 * for a top-level "query" field would misfire on ordinary search APIs.
 */
export function classifyGraphql(req: Request): Action | undefined {
  const path = req.path.split("?")[0].replace(/\/+$/, "");
  if (!path.endsWith("/graphql")) return undefined;
  let text = bodyText(req);
  try {
    const obj = JSON.parse(text);
    if (obj && typeof obj === "object" && typeof obj.query === "string") text = obj.query;
  } catch {
    // not JSON; fall back to the raw body text
  }
  // Coarse: a top-level `mutation` keyword means a write. Real gating would
  // tokenise the query; this is enough to split reads from writes.
  const isWrite = /\bmutation\b/.test(text);
  return {
    service: "graphql",
    verb: isWrite ? "graphql:mutation" : "graphql:query",
    kind: isWrite ? WRITE : READ,
    facet: { name: "graphql", fields: { operation: isWrite ? "mutation" : "query" } },
  };
}

export function classifyGithub(req: Request): Action {
  const gql = classifyGraphql(req);
  if (gql) return { ...gql, service: "github" };
  const path = req.path.split("?")[0];
  const verb = req.method.toUpperCase();
  const kind: Kind = verb === "GET" || verb === "HEAD" ? READ : WRITE;
  return { service: "github", verb: `${verb} ${path}`, kind };
}

export function classify(
  req: Request,
  k8sHosts: Iterable<string> = [],
  plugins: readonly Classifier[] = [],
): Action {
  const host = req.host.toLowerCase();
  // Plugins outrank built-ins so an operator can sharpen (or replace) the stock
  // classification for a host. First match in load order wins; undefined falls
  // through — so a failed-to-load plugin leaves its hosts on the built-in chain,
  // where anything unrecognised classifies UNKNOWN and fails closed in decide().
  for (const p of plugins) {
    if (!hostMatches(host, p.hosts)) continue;
    const action = p.classify(req);
    if (action) return action;
  }
  if (hostMatches(host, k8sHosts)) return classifyK8s(req);
  if (host === "amazonaws.com" || host.endsWith(".amazonaws.com")) return classifyAws(req);
  if (host === "github.com" || host.endsWith(".github.com")) return classifyGithub(req);
  return classifyGraphql(req) ?? {
    service: "other",
    verb: `${req.method.toUpperCase()} ${req.path.split("?")[0]}`,
    kind: UNKNOWN,
  };
}

// ---- decision --------------------------------------------------------------

export function decide(
  req: Request,
  pol: Policy,
  plugins: readonly Classifier[] = [],
): Decision {
  const host = req.host.toLowerCase();
  const unknownAction = { service: "?", verb: "?", kind: UNKNOWN } as const;

  if (hostMatches(host, pol.denyHosts)) {
    return { verdict: "deny", reason: `host ${host} explicitly denied`, action: unknownAction };
  }
  // A host claimed by an ACTIVE plugin skips the allowlist gate: enabling the plugin
  // for this scope IS the trust decision for its hosts, and it's stricter than a bare
  // allowHosts entry — the plugin's table classifies every request, unmatched ops are
  // UNKNOWN (deny/hold by mode), and when the activation expires the hostMiss gate
  // below takes over again. denyHosts still wins outright (above).
  const pluginClaimed = plugins.some((p) => hostMatches(host, p.hosts));
  if (!pluginClaimed && pol.allowHosts.size > 0 && !hostMatches(host, pol.allowHosts)) {
    const reason = pol.hostMiss === "hold"
      ? `host ${host} not in allowlist — approval needed`
      : pol.hostMiss === "allow"
      ? `host ${host} not in allowlist — allowed`
      : `host ${host} not in allowlist`;
    return { verdict: pol.hostMiss, reason, action: unknownAction };
  }

  const action = classify(req, pol.k8sHosts, plugins);

  // Condition rules — the sharpest layer, ordered, first match wins (upstream Claw
  // Patrol semantics). A rule whose condition can't evaluate DENIES (fail closed):
  // an unevaluable condition must never fall through to a looser layer.
  const env = ruleEnv(req, action);
  for (const rule of pol.rules) {
    if (rule.hosts?.length && !hostMatches(host, rule.hosts)) continue;
    let matched: boolean;
    try {
      matched = rule.test(env);
    } catch (e) {
      return {
        verdict: "deny",
        reason: `rule ${rule.name}: condition unevaluable (${
          (e as Error).message
        }) — failing closed`,
        action,
        rule: rule.name,
      };
    }
    if (!matched) continue;
    if (rule.approve?.length) {
      return {
        verdict: "hold",
        reason: rule.reason ?? `rule ${rule.name}: approval required`,
        action,
        approve: rule.approve,
        rule: rule.name,
      };
    }
    return {
      verdict: rule.verdict!,
      reason: rule.reason ?? `rule ${rule.name} matched`,
      action,
      rule: rule.name,
    };
  }

  // Explicit per-verb overrides. Deny wins over hold wins over allow.
  if (pol.denyVerbs.has(action.verb)) {
    return { verdict: "deny", reason: `${action.verb} explicitly denied`, action };
  }
  if (pol.holdVerbs.has(action.verb)) {
    return { verdict: "hold", reason: `${action.verb} needs approval`, action };
  }
  if (pol.allowVerbs.has(action.verb)) {
    return { verdict: "allow", reason: `${action.verb} explicitly allowed`, action };
  }

  if (pol.mode === "all") {
    return { verdict: "allow", reason: "host allowed; mode=all", action };
  }

  // reads pass in every gated mode.
  if (action.kind === READ) {
    return { verdict: "allow", reason: `read action ${action.verb}`, action };
  }

  // mode == "review": writes and unknowns are held for human approval.
  if (pol.mode === "review") {
    const what = action.kind === WRITE ? "write" : "unknown";
    return { verdict: "hold", reason: `${what} action ${action.verb} needs approval`, action };
  }

  // mode == "read_only": writes blocked, unknown fails closed.
  if (action.kind === WRITE) {
    return {
      verdict: "deny",
      reason: `write action ${action.verb} blocked (mode=read_only)`,
      action,
    };
  }
  return { verdict: "deny", reason: `unknown action ${action.verb} blocked (fail closed)`, action };
}
