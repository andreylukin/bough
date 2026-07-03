/**
 * The egress gate — the seam the native proxy (proxy.ts) calls for every outbound
 * request the sandbox makes. It ties the pure policy brain (policy.ts) to the running
 * server: classify → decide → persist a NetRequest row → emit a `net.request` event →
 * (if held) block until a human resolves it.
 *
 *   gate(req) ──decide──▶ allow/deny  → persist+emit, resolve immediately
 *                     └─▶ hold        → persist+emit (verdict "pending"),
 *                                       await POST /net/requests/:id/{allow,deny},
 *                                       re-emit with the final verdict, resolve allow/deny
 *
 * The awaited Promise is the "hold-and-ask" gate: the proxy is literally paused on the
 * wire until the operator approves, so nothing egresses without a say. Wire verdicts map
 * to the UI's NetRequest: allow→allowed, deny→denied, hold→pending. Persistence + emit go
 * through Db + Bus so a reconnecting UI can rebuild the rail from /net/requests and
 * live-update from /events. The policy is swappable at runtime (setPolicy) so the rule-set
 * editor takes effect on the next request without a restart.
 */
import {
  decide,
  type Decision,
  type Policy,
  policy as makePolicy,
  type Request,
} from "./policy.ts";
import type { ExtensionHost } from "./extensions.ts";
import type { Db } from "../db/db.ts";
import type { Bus } from "../bus.ts";
import type { NetRequest } from "../schema/parts.ts";

const WIRE = { allow: "allowed", deny: "denied", hold: "pending" } as const;

export interface GateOpts {
  /** Session the request belongs to (rows/events are filtered by it in the UI). */
  sessionId?: string;
  /** Who made the request — a tool name, "worker", etc. */
  requestedBy?: string;
}

interface Hold {
  resolve: (approve: boolean) => void;
  request: NetRequest;
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
  /** Programmable guards (extensions.ts); a guard verdict overrides the static one. */
  #extensions?: ExtensionHost;

  constructor(cfg: {
    policy: Policy;
    db: Db;
    bus: Bus;
    resolve?: (sessionId?: string) => Policy;
    extensions?: ExtensionHost;
  }) {
    this.#policy = cfg.policy;
    this.#db = cfg.db;
    this.#bus = cfg.bus;
    this.#resolve = cfg.resolve;
    this.#extensions = cfg.extensions;
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
    let decision = decide(req, this.#policyFor(opts.sessionId));
    // Programmable guards see the request + the static decision; the first guard
    // verdict overrides it (a broken/slow guard falls through inside the host).
    // No-extensions stays fully synchronous up to the hold registration, so a
    // caller can observe `pending` right after calling gate().
    const override = this.#extensions
      ? await this.#extensions.gate(req, decision, opts.sessionId)
      : undefined;
    if (override) {
      decision = { verdict: override.verdict, reason: override.reason, action: decision.action };
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
      ts: Date.now(),
    };
    this.#emit(record, opts.sessionId);

    if (decision.verdict !== "hold") return decision;

    // Hold: park the caller until a human resolves this id.
    return new Promise<boolean>((resolve) => {
      this.#holds.set(record.id, { resolve, request: record });
    }).then((approve) => {
      const final: Decision = approve
        ? { verdict: "allow", reason: "approved by human", action: decision.action }
        : { verdict: "deny", reason: "denied by human", action: decision.action };
      this.#emit(
        { ...record, verdict: WIRE[final.verdict], reason: final.reason, ts: Date.now() },
        opts.sessionId,
      );
      return final;
    });
  }

  /** Resolve a held request. Returns false if the id isn't awaiting approval. */
  resolveHold(id: string, approve: boolean): boolean {
    const hold = this.#holds.get(id);
    if (!hold) return false;
    this.#holds.delete(id);
    hold.resolve(approve);
    return true;
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
    extensions?: ExtensionHost;
  },
): Gate {
  return new Gate({
    policy: cfg.policy ?? makePolicy(),
    db: cfg.db,
    bus: cfg.bus,
    resolve: cfg.resolve,
    extensions: cfg.extensions,
  });
}
