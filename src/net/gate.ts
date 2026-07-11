/**
 * The egress gate — the seam the native proxy (proxy.ts) calls for every outbound
 * request the sandbox makes. It ties the pure policy brain (policy.ts) to the running
 * server: classify → decide → persist a NetRequest row → emit a `net.request` event →
 * (if held) block until a human resolves it.
 *
 *   gate(req) ──decide──▶ allow/deny  → persist+emit, resolve immediately
 *                     └─▶ hold        → persist+emit (verdict "pending"), run the
 *                                       APPROVER CHAIN — every approver must allow —
 *                                       re-emit with the final verdict, resolve allow/deny
 *
 * A held decision carries its approver chain (decision.approve, from a matched rule;
 * default ["human"]). Approvers run in order and EACH must allow:
 *   - "human"          — park until POST /net/requests/:id/{allow,deny} (the classic
 *                        hold-and-ask; the proxy is literally paused on the wire);
 *   - "plugin:<name>"  — the named plugin's gate() hook, consulted as an approver
 *                        (plugins.ts runApprover). Fail-closed: only an explicit
 *                        allow passes — deny, abstain, throw, timeout, an inactive
 *                        plugin, or a host outside the plugin's claim all deny.
 * Plugins never override the rule set on their own — a rule must route to them
 * (upstream Claw Patrol's "plugins must not decide" principle).
 *
 * Wire verdicts map to the UI's NetRequest: allow→allowed, deny→denied, hold→pending.
 * Persistence + emit go through Db + Bus so a reconnecting UI can rebuild the rail from
 * /net/requests and live-update from /events. The policy is swappable at runtime
 * (setPolicy) so the rule-set editor takes effect on the next request without a restart.
 */
import {
  type Classifier,
  decide,
  type Decision,
  hostMatches,
  type Policy,
  policy as makePolicy,
  type Request,
} from "./policy.ts";
import { type PluginGuard, runApprover, type RunGuardOpts } from "./plugins.ts";
import type { Db } from "../db/db.ts";
import type { Bus } from "../bus.ts";
import type { NetRequest } from "../schema/parts.ts";

const WIRE = { allow: "allowed", deny: "denied", hold: "pending" } as const;

export interface GateOpts {
  /** Session the request belongs to (rows/events are filtered by it in the UI). */
  sessionId?: string;
  /** Who made the request — a tool name, "worker", etc. */
  requestedBy?: string;
  /**
   * Extra classifiers consulted BEFORE the session's plugin classifiers — how a
   * non-HTTP caller (mcp/gate.ts) teaches decide() its pseudo-request vocabulary.
   * A claimed host also skips the allowlist gate, same as an active plugin's.
   */
  classifiers?: readonly Classifier[];
}

interface Hold {
  resolve: (out: { approve: boolean; reason?: string }) => void;
  request: NetRequest;
  sessionId?: string;
  /**
   * True once the parked socket has already been failed closed by the hold timeout.
   * The approval card stays live (re-requestable): a later approve mints a session
   * grant so the RETRIED command succeeds; the dead socket is never resumed.
   */
  detached: boolean;
}

/** A short-TTL, session-scoped allowance minted when a human approves a held request. */
interface Grant {
  sessionId?: string;
  host: string;
  /** The classified action verb the grant covers (NetRequest.action). */
  verb: string;
  expires: number;
}

/** How long a human hold parks before failing closed with a re-request message. */
const DEFAULT_HOLD_TIMEOUT_MS = 120_000;
/** Lifetime of a session grant minted on approval (short — retry-then-expire). */
const DEFAULT_GRANT_TTL_MS = 5 * 60_000;

export class Gate {
  #policy: Policy;
  #db: Db;
  #bus: Bus;
  #holds = new Map<string, Hold>();
  /** Per-session policy lookup (branch overrides). Absent = #policy for everyone. */
  #resolve?: (sessionId?: string) => Policy;
  /** Compiled-policy cache keyed by sessionId; cleared whenever any rule set changes. */
  #cache = new Map<string, Policy>();
  /**
   * Live plugin classifiers for the session that owns the request (activation is
   * per-branch — plugins.ts activeFor). Read per request so hot-reload, activation
   * edits, and per-activation TTL apply on the next gate() without invalidation.
   */
  #classifiers?: (sessionId?: string) => readonly Classifier[];
  /** Plugin gate() hooks for the owning session — approve-chain lookup (#runChain). */
  #guards?: (sessionId?: string) => readonly PluginGuard[];
  #guardOpts: RunGuardOpts;
  /**
   * Advisory one-liner for held requests (production: the local worker —
   * worker/annotate.ts). Absent = no annotations; nothing gates on it.
   */
  #annotator?: (record: NetRequest) => Promise<string | null>;
  /** Human-hold timeout; 0 disables it (hermetic tests, explicit opt-out). */
  #holdTimeoutMs: number;
  #grantTtlMs: number;
  /** Live session grants minted on approval; checked before a non-allow verdict stands. */
  #grants: Grant[] = [];

  constructor(cfg: {
    policy: Policy;
    db: Db;
    bus: Bus;
    resolve?: (sessionId?: string) => Policy;
    classifiers?: (sessionId?: string) => readonly Classifier[];
    guards?: (sessionId?: string) => readonly PluginGuard[];
    guardOpts?: RunGuardOpts;
    annotator?: (record: NetRequest) => Promise<string | null>;
    holdTimeoutMs?: number;
    grantTtlMs?: number;
  }) {
    this.#policy = cfg.policy;
    this.#db = cfg.db;
    this.#bus = cfg.bus;
    this.#resolve = cfg.resolve;
    this.#classifiers = cfg.classifiers;
    this.#guards = cfg.guards;
    this.#guardOpts = cfg.guardOpts ?? {};
    this.#annotator = cfg.annotator;
    this.#holdTimeoutMs = cfg.holdTimeoutMs ?? DEFAULT_HOLD_TIMEOUT_MS;
    this.#grantTtlMs = cfg.grantTtlMs ?? DEFAULT_GRANT_TTL_MS;
  }

  /** Number of requests currently awaiting human approval (introspection/tests). */
  get pending(): number {
    return this.#holds.size;
  }

  /** Swap the enforced policy (rule-set editor). Affects the next request; in-flight holds keep theirs. */
  setPolicy(policy: Policy): void {
    this.#policy = policy;
    this.#cache.clear();
  }

  /** Drop cached compiled policies after a per-session rule edit (PUT/DELETE with ?session=). */
  invalidate(): void {
    this.#cache.clear();
  }

  #policyFor(sessionId?: string): Policy {
    if (!this.#resolve) return this.#policy;
    const key = sessionId ?? "";
    let p = this.#cache.get(key);
    if (!p) {
      p = this.#resolve(sessionId);
      this.#cache.set(key, p);
    }
    return p;
  }

  /**
   * Gate one outbound request. Resolves once the verdict is final: allow/deny is
   * immediate; a hold resolves when approved or denied via resolveHold
   * (POST /net/requests/:id/{allow,deny}).
   */
  async gate(req: Request, opts: GateOpts = {}): Promise<Decision> {
    let decision = decide(
      req,
      this.#policyFor(opts.sessionId),
      [...(opts.classifiers ?? []), ...(this.#classifiers?.(opts.sessionId) ?? [])],
    );
    // A live session grant (minted when a human approved this host+verb earlier) turns
    // an otherwise-held/denied request into an allow, so the RETRY after an approval
    // succeeds without re-prompting.
    if (decision.verdict !== "allow" && this.#granted(req, decision, opts.sessionId)) {
      decision = { ...decision, verdict: "allow", reason: "approved for this session (grant)" };
    }
    const record: NetRequest = {
      id: crypto.randomUUID(),
      sessionId: opts.sessionId,
      host: req.host,
      verb: req.method,
      action: decision.action.verb,
      verdict: WIRE[decision.verdict],
      reason: decision.reason,
      requestedBy: opts.requestedBy,
      fields: decision.action.facet?.fields,
      ts: Date.now(),
    };
    this.#emit(record, opts.sessionId);

    if (decision.verdict !== "hold") return decision;

    // Fire-and-forget: annotate the parked request while the human looks at it.
    this.#annotate(record, opts.sessionId);
    // Hold: run the approver chain (default: a human), then re-emit the final verdict
    // on the same id so the approval card updates in place.
    const final = await this.#runChain(decision, record, req, opts.sessionId);
    if (final.timedOut) {
      // Fail THIS socket closed but leave the card pending + re-requestable (the
      // detached hold survives; approving it later mints a grant so the retry passes).
      this.#emit({ ...record, reason: final.reason, ts: Date.now() }, opts.sessionId);
      return final;
    }
    this.#emit(
      { ...record, verdict: WIRE[final.verdict], reason: final.reason, ts: Date.now() },
      opts.sessionId,
    );
    return final;
  }

  /**
   * Run a held decision's approver chain in order; EVERY approver must allow.
   * "human" parks the caller until resolveHold (or expireHolds sweeps it); a
   * "plugin:<name>" approver is the plugin's gate() hook, and anything short of an
   * explicit allow — deny, abstain, throw, timeout, plugin not active for this scope,
   * host outside the plugin's claim — denies. Fail-closed all the way down.
   */
  async #runChain(
    decision: Decision,
    record: NetRequest,
    req: Request,
    sessionId?: string,
  ): Promise<Decision & { timedOut?: boolean }> {
    const { action } = decision;
    const chain = decision.approve ?? ["human"];
    // Single approver: its own reason reads best. A longer chain: name every step.
    const solo = chain.length === 1;
    let allowReason = solo && chain[0] === "human"
      ? "approved by human"
      : `approved by chain: ${chain.join(" → ")}`;
    for (const approver of chain) {
      if (approver === "human") {
        const { approve, reason, timedOut } = await new Promise<
          { approve: boolean; reason?: string; timedOut?: boolean }
        >((resolve) => {
          const timer = this.#holdTimeoutMs > 0
            ? setTimeout(() => {
              // Detach: the card stays live for later approval; the socket fails closed.
              const h = this.#holds.get(record.id);
              if (h) h.detached = true;
              resolve({ approve: false, timedOut: true });
            }, this.#holdTimeoutMs)
            : undefined;
          this.#holds.set(record.id, {
            resolve: (out) => {
              if (timer !== undefined) clearTimeout(timer);
              resolve(out);
            },
            request: record,
            sessionId,
            detached: false,
          });
        });
        if (timedOut) {
          return {
            verdict: "deny",
            reason: "held for approval — approve in the Network rail and retry",
            action,
            timedOut: true,
          };
        }
        if (!approve) return { verdict: "deny", reason: reason ?? "denied by human", action };
        if (solo && reason) allowReason = reason;
        continue;
      }
      if (!approver.startsWith("plugin:")) {
        return { verdict: "deny", reason: `unknown approver ${approver} — failing closed`, action };
      }
      const name = approver.slice("plugin:".length);
      const guard = (this.#guards?.(sessionId) ?? []).find((g) => g.name === name);
      if (!guard) {
        return {
          verdict: "deny",
          reason: `approver ${approver} is not active for this scope — failing closed`,
          action,
        };
      }
      if (!hostMatches(req.host.toLowerCase(), guard.hosts)) {
        return {
          verdict: "deny",
          reason: `approver ${approver} does not claim host ${req.host} — failing closed`,
          action,
        };
      }
      const out = await runApprover(guard, req, decision, sessionId, this.#guardOpts);
      if (out?.verdict !== "allow") {
        return {
          verdict: "deny",
          reason: out?.reason ?? `approver ${approver} did not allow — failing closed`,
          action,
        };
      }
      if (solo && out.reason) allowReason = out.reason;
    }
    return { verdict: "allow", reason: allowReason, action };
  }

  /**
   * Resolve a held request. `scope` "session" (and any approval of a timed-out,
   * detached hold) mints a short-TTL session grant so the retried command passes
   * without re-prompting; "once" just releases the still-parked socket. Returns false
   * if the id isn't awaiting approval.
   */
  resolveHold(id: string, approve: boolean, scope: "once" | "session" = "once"): boolean {
    const hold = this.#holds.get(id);
    if (!hold) return false;
    this.#holds.delete(id);
    if (approve && (scope === "session" || hold.detached)) {
      this.#grant(hold.sessionId, hold.request.host, hold.request.action);
    }
    if (hold.detached) {
      // The socket already 403'd on timeout; settle the lingering card. On approval the
      // grant above lets the agent's retry through — nothing to resume here.
      this.#emit({
        ...hold.request,
        verdict: approve ? "allowed" : "denied",
        reason: approve ? "approved — retry to proceed" : "denied",
        ts: Date.now(),
      }, hold.sessionId);
    } else {
      hold.resolve({ approve });
    }
    return true;
  }

  /** Record a session grant (host + classified verb) with a short TTL. */
  #grant(sessionId: string | undefined, host: string, verb: string): void {
    this.#grants = this.#grants.filter((g) => g.expires > Date.now());
    this.#grants.push({ sessionId, host, verb, expires: Date.now() + this.#grantTtlMs });
  }

  /** Whether a live grant covers this request's host + classified verb for its session. */
  #granted(req: Request, decision: Decision, sessionId?: string): boolean {
    const now = Date.now();
    return this.#grants.some((g) =>
      g.expires > now && g.sessionId === sessionId &&
      hostMatches(req.host.toLowerCase(), [g.host.toLowerCase()]) &&
      g.verb === decision.action.verb
    );
  }

  /**
   * Deny-and-clear parked holds whose turn is gone — for one session, or all of
   * them (sessionId undefined). Without this, an interrupted turn leaves its hold
   * pending forever and the approval card haunts every session. Returns the count.
   */
  expireHolds(sessionId: string | undefined, reason: string): number {
    let n = 0;
    for (const [id, hold] of [...this.#holds]) {
      if (sessionId !== undefined && hold.sessionId !== sessionId) continue;
      // Detached holds have no live socket to fail — they persist as re-requestable
      // cards (the timeout already 403'd the request) until a human acts on them.
      if (hold.detached) continue;
      this.#holds.delete(id);
      hold.resolve({ approve: false, reason });
      n++;
    }
    return n;
  }

  /**
   * Ask the annotator for a one-liner and re-emit the held card with it. Mutating
   * `record` lets a late annotation still ride out on the final-verdict re-emit
   * (same object); the in-place pending re-emit happens only while the hold is
   * still parked, so an annotation can never regress a decided card.
   */
  #annotate(record: NetRequest, sessionId?: string): void {
    if (!this.#annotator) return;
    this.#annotator(record).then((summary) => {
      if (!summary) return;
      record.annotation = summary;
      if (this.#holds.has(record.id)) this.#emit({ ...record }, sessionId);
    }).catch(() => {});
  }

  #emit(record: NetRequest, sessionId?: string): void {
    this.#db.recordNetEvent(sessionId, record);
    this.#bus.publish({ type: "net.request", sessionId, data: record });
  }
}

/** Build a Gate with the given policy (default: host-open, read-only — see policy.ts). */
export function createGate(
  cfg: {
    db: Db;
    bus: Bus;
    policy?: Policy;
    resolve?: (sessionId?: string) => Policy;
    classifiers?: (sessionId?: string) => readonly Classifier[];
    guards?: (sessionId?: string) => readonly PluginGuard[];
    guardOpts?: RunGuardOpts;
    annotator?: (record: NetRequest) => Promise<string | null>;
    holdTimeoutMs?: number;
    grantTtlMs?: number;
  },
): Gate {
  return new Gate({
    policy: cfg.policy ?? makePolicy(),
    db: cfg.db,
    bus: cfg.bus,
    resolve: cfg.resolve,
    classifiers: cfg.classifiers,
    guards: cfg.guards,
    guardOpts: cfg.guardOpts,
    annotator: cfg.annotator,
    holdTimeoutMs: cfg.holdTimeoutMs,
    grantTtlMs: cfg.grantTtlMs,
  });
}
