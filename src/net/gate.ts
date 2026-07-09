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
}

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

  constructor(cfg: {
    policy: Policy;
    db: Db;
    bus: Bus;
    resolve?: (sessionId?: string) => Policy;
    classifiers?: (sessionId?: string) => readonly Classifier[];
    guards?: (sessionId?: string) => readonly PluginGuard[];
    guardOpts?: RunGuardOpts;
    annotator?: (record: NetRequest) => Promise<string | null>;
  }) {
    this.#policy = cfg.policy;
    this.#db = cfg.db;
    this.#bus = cfg.bus;
    this.#resolve = cfg.resolve;
    this.#classifiers = cfg.classifiers;
    this.#guards = cfg.guards;
    this.#guardOpts = cfg.guardOpts ?? {};
    this.#annotator = cfg.annotator;
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
    const decision = decide(
      req,
      this.#policyFor(opts.sessionId),
      [...(opts.classifiers ?? []), ...(this.#classifiers?.(opts.sessionId) ?? [])],
    );
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
  ): Promise<Decision> {
    const { action } = decision;
    const chain = decision.approve ?? ["human"];
    // Single approver: its own reason reads best. A longer chain: name every step.
    const solo = chain.length === 1;
    let allowReason = solo && chain[0] === "human"
      ? "approved by human"
      : `approved by chain: ${chain.join(" → ")}`;
    for (const approver of chain) {
      if (approver === "human") {
        const { approve, reason } = await new Promise<{ approve: boolean; reason?: string }>(
          (resolve) => {
            this.#holds.set(record.id, { resolve, request: record, sessionId });
          },
        );
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

  /** Resolve a held request. Returns false if the id isn't awaiting approval. */
  resolveHold(id: string, approve: boolean): boolean {
    const hold = this.#holds.get(id);
    if (!hold) return false;
    this.#holds.delete(id);
    hold.resolve({ approve });
    return true;
  }

  /**
   * Approve-and-release parked holds whose branch now resolves to mode "yolo" —
   * called right after the YOLO toggle flips on, so requests already queued for
   * approval don't stay parked on a branch that no longer gates anything.
   * Scoping rides on policy resolution itself: a child branch inherits its
   * parent's yolo, while one carrying its own non-yolo override keeps its holds.
   * Returns the count released.
   */
  releaseYoloHolds(): number {
    let n = 0;
    for (const [id, hold] of [...this.#holds]) {
      if (this.#policyFor(hold.sessionId).mode !== "yolo") continue;
      this.#holds.delete(id);
      hold.resolve({ approve: true, reason: "auto-approved: YOLO is on for this branch" });
      n++;
    }
    return n;
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
  });
}
