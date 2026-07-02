/**
 * The egress gate — the seam the Claw Patrol event stream / mitm addon calls for every
 * outbound request the sandbox makes. It ties the pure policy brain (policy.ts) to the
 * running server: classify → decide → persist a NetRequest row → emit a `net.request`
 * event → (if held) block until a human resolves it.
 *
 *   gate(req) ──decide──▶ allow/deny  → persist+emit, return immediately
 *                     └─▶ hold        → persist+emit (verdict "pending"),
 *                                       await POST /net/requests/:id/{allow,deny},
 *                                       re-emit with the final verdict, return allow/deny
 *
 * The awaited Promise is the "hold-and-ask" gate: the caller (the gateway addon) is
 * literally paused on the wire until the Driver approves, so nothing egresses without a
 * say. Wire verdicts map to the UI's NetRequest: allow→allowed, deny→denied,
 * hold→pending. Persistence + emit go through NetStore + Bus so a reconnecting UI can
 * rebuild the rail from /net/requests and live-update from /events.
 */
import { decide, type Decision, type Policy, policy as makePolicy, type Request } from "./policy.ts";
import type { NetStore } from "../db/net.ts";
import type { Bus } from "../bus.ts";
import type { NetRequest } from "../schema/parts.ts";

const WIRE = { allow: "allowed", deny: "denied", hold: "pending" } as const;

export interface GateOpts {
  /** Session the request belongs to (rows/events are filtered by it in the UI). */
  sessionId?: string;
  /** Who made the request — "worker", "supervisor", a tool name. */
  requestedBy?: string;
}

interface Hold {
  resolve: (approve: boolean) => void;
  sessionId?: string;
  request: NetRequest;
}

export interface GateConfig {
  policy: Policy;
  netStore: NetStore;
  bus: Bus;
}

export class Gate {
  #policy: Policy;
  #net: NetStore;
  #bus: Bus;
  #holds = new Map<string, Hold>();

  constructor(cfg: GateConfig) {
    this.#policy = cfg.policy;
    this.#net = cfg.netStore;
    this.#bus = cfg.bus;
  }

  /** Number of requests currently awaiting human approval (introspection/tests). */
  get pending(): number {
    return this.#holds.size;
  }

  /**
   * Gate one outbound request. Resolves once the verdict is final: for allow/deny that
   * is immediate; for a hold it is when the request is approved or denied via
   * resolveHold (POST /net/requests/:id/{allow,deny}).
   */
  gate(req: Request, opts: GateOpts = {}): Promise<Decision> {
    const decision = decide(req, this.#policy);
    const record: NetRequest = {
      id: crypto.randomUUID(),
      host: req.host,
      verb: req.method,
      action: decision.action.verb,
      verdict: WIRE[decision.verdict],
      reason: decision.reason,
      requestedBy: opts.requestedBy,
      ts: Date.now(),
    };
    this.#emit(record, opts.sessionId);

    if (decision.verdict !== "hold") return Promise.resolve(decision);

    // Hold: park the caller until a human resolves this id.
    return new Promise<boolean>((resolve) => {
      this.#holds.set(record.id, { resolve, sessionId: opts.sessionId, request: record });
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
    this.#net.upsert(sessionId, record);
    this.#bus.publish({ type: "net.request", sessionId, data: record });
  }
}

/** Build a Gate with the default (host-open, read-only) policy unless one is given. */
export function createGate(cfg: { netStore: NetStore; bus: Bus; policy?: Policy }): Gate {
  return new Gate({ policy: cfg.policy ?? makePolicy(), netStore: cfg.netStore, bus: cfg.bus });
}
