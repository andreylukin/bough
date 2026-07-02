/**
 * The egress firewall bough runs in-process — native Claw Patrol. It owns a MITM
 * certificate authority (ca.ts), the policy gate (gate.ts), and the intercepting proxy
 * (proxy.ts), and it hands the sandbox exec path the env that routes commands through
 * the proxy and trusts its CA. No Go binary, no WireGuard, no external dashboard: the
 * audit feed + human approvals live on bough's own /net/requests + Network rail.
 *
 * Opt-in for now (BOUGH_CLAWPATROL=1). The default flips to on once the approval UI can
 * clear held requests (until then a fail-closed default could wedge a turn with no way
 * to unblock it). With the flag off, the proxy never starts and exec runs unrouted.
 */
import { caEnv, CertAuthority } from "./ca.ts";
import { ProxyServer } from "./proxy.ts";
import { createGate, type Gate } from "./gate.ts";
import { loadConfig, toPolicy } from "./config.ts";
import type { Db } from "../db/db.ts";
import type { Bus } from "../bus.ts";

/** Whether bough should run the egress proxy (opt-in — see module docs). */
export function clawpatrolEnabled(): boolean {
  return Deno.env.get("BOUGH_CLAWPATROL") === "1";
}

export interface GatewayStatus {
  enabled: boolean;
  running: boolean;
  proxyUrl: string;
  caPath: string;
}

/** Owns the CA + gate + proxy for the server's lifetime. */
export class ClawpatrolGateway {
  #db: Db;
  #bus: Bus;
  #ca?: CertAuthority;
  #gate?: Gate;
  #proxy?: ProxyServer;
  #running = false;

  constructor(cfg: { db: Db; bus: Bus }) {
    this.#db = cfg.db;
    this.#bus = cfg.bus;
  }

  /** The gate the server puts on AppCtx so the approval endpoints share it with the proxy. */
  get gate(): Gate | undefined {
    return this.#gate;
  }

  status(): GatewayStatus {
    return {
      enabled: clawpatrolEnabled(),
      running: this.#running,
      proxyUrl: this.#proxy?.url ?? "",
      caPath: this.#ca?.caCertPath ?? "",
    };
  }

  /** Boot the CA, gate, and proxy when enabled. No-op (exec runs unrouted) when off. */
  async start(): Promise<void> {
    if (!clawpatrolEnabled()) return;
    this.#ca = CertAuthority.load();
    this.#gate = createGate({ db: this.#db, bus: this.#bus, policy: toPolicy(loadConfig()) });
    const gate = this.#gate;
    this.#proxy = new ProxyServer({
      ca: this.#ca,
      gate: (req, opts) => gate.gate(req, opts),
    });
    await this.#proxy.start();
    this.#running = true;
    console.log(`[clawpatrol] native egress proxy on ${this.#proxy.url}`);
    console.log(`[clawpatrol] sandbox clients trust the MITM CA at ${this.#ca.caCertPath}`);
  }

  async stop(): Promise<void> {
    this.#running = false;
    await this.#proxy?.stop();
    this.#proxy = undefined;
  }

  /**
   * Env for a sandboxed command: point its HTTP(S) client at the proxy and trust the
   * MITM CA. NO_PROXY keeps loopback (bough's own server, and the proxy itself) direct
   * so requests don't loop. Empty when the proxy isn't running (exec runs unrouted).
   */
  env(): Record<string, string> {
    if (!this.#running || !this.#proxy || !this.#ca) return {};
    const url = this.#proxy.url;
    return {
      HTTP_PROXY: url,
      HTTPS_PROXY: url,
      http_proxy: url,
      https_proxy: url,
      NO_PROXY: "localhost,127.0.0.1",
      no_proxy: "localhost,127.0.0.1",
      ...caEnv(this.#ca.caCertPath),
    };
  }
}

// The process-wide gateway, set by the server on boot so the exec path (bash.ts) can
// reach it without threading it through every tool signature. Undefined in tests.
let active: ClawpatrolGateway | undefined;
export function setActiveGateway(g: ClawpatrolGateway | undefined): void {
  active = g;
}
export function activeGateway(): ClawpatrolGateway | undefined {
  return active;
}

/** The sandbox-exec env from the active gateway; empty when the proxy isn't running. */
export function clawpatrolEnv(): Record<string, string> {
  return active?.env() ?? {};
}
