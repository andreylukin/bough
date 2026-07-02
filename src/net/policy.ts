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
  /** "aws:ec2", "k8s", "github", "other". */
  service: string;
  /** e.g. "TerminateInstances", "DELETE /api/v1/pods/x", "graphql:mutation". */
  verb: string;
  kind: Kind;
}

export interface Decision {
  verdict: Verdict;
  reason: string;
  action: Action;
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

function bodyText(req: Request): string {
  const b = req.body;
  if (b == null) return "";
  if (typeof b === "string") return b;
  return new TextDecoder().decode(b);
}

function hostMatches(host: string, patterns: Iterable<string>): boolean {
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
    return { service, verb: op, kind: awsKind(op) };
  }
  // query protocol: Action= in the body, else in the path's query string.
  const qs = req.path.includes("?") ? req.path.split("?").slice(1).join("?") : "";
  for (const source of [bodyText(req), qs]) {
    const action = firstQueryValue(source, "Action");
    if (action) return { service, verb: action, kind: awsKind(action) };
  }
  return { service, verb: "?", kind: UNKNOWN };
}

export function classifyK8s(req: Request): Action {
  const verb = req.method.toUpperCase();
  const kind: Kind = verb === "GET" || verb === "HEAD"
    ? READ
    : ["POST", "PUT", "PATCH", "DELETE"].includes(verb)
    ? WRITE
    : UNKNOWN;
  const resource = req.path.split("?")[0];
  return { service: "k8s", verb: `${verb} ${resource}`, kind };
}

export function classifyGithub(req: Request): Action {
  const path = req.path.split("?")[0];
  if (path.replace(/\/+$/, "").endsWith("/graphql")) {
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
      service: "github",
      verb: isWrite ? "graphql:mutation" : "graphql:query",
      kind: isWrite ? WRITE : READ,
    };
  }
  const verb = req.method.toUpperCase();
  const kind: Kind = verb === "GET" || verb === "HEAD" ? READ : WRITE;
  return { service: "github", verb: `${verb} ${path}`, kind };
}

export function classify(req: Request, k8sHosts: Iterable<string> = []): Action {
  const host = req.host.toLowerCase();
  if (hostMatches(host, k8sHosts)) return classifyK8s(req);
  if (host === "amazonaws.com" || host.endsWith(".amazonaws.com")) return classifyAws(req);
  if (host === "github.com" || host.endsWith(".github.com")) return classifyGithub(req);
  return {
    service: "other",
    verb: `${req.method.toUpperCase()} ${req.path.split("?")[0]}`,
    kind: UNKNOWN,
  };
}

// ---- decision --------------------------------------------------------------

export function decide(req: Request, pol: Policy): Decision {
  const host = req.host.toLowerCase();
  const unknownAction = { service: "?", verb: "?", kind: UNKNOWN } as const;

  if (hostMatches(host, pol.denyHosts)) {
    return { verdict: "deny", reason: `host ${host} explicitly denied`, action: unknownAction };
  }
  if (pol.allowHosts.size > 0 && !hostMatches(host, pol.allowHosts)) {
    const reason = pol.hostMiss === "hold"
      ? `host ${host} not in allowlist — approval needed`
      : pol.hostMiss === "allow"
      ? `host ${host} not in allowlist — allowed`
      : `host ${host} not in allowlist`;
    return { verdict: pol.hostMiss, reason, action: unknownAction };
  }

  const action = classify(req, pol.k8sHosts);

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
