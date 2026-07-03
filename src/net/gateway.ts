/**
 * The egress firewall bough runs in-process — native Claw Patrol. It owns a MITM
 * certificate authority (ca.ts), the policy gate (gate.ts), and one intercepting proxy
 * (proxy.ts) PER SESSION, and it hands the sandbox exec path the env that routes
 * commands through their session's proxy and trusts the shared CA. No Go binary, no
 * WireGuard, no external dashboard: the audit feed + human approvals live on bough's
 * own /net/requests + Network rail.
 *
 * Per-session listeners are how egress gets attributed and policied by branch: the
 * proxy can only tell sessions apart by something on the wire, and the listening port
 * is that signal. Listeners are spun up lazily on a session's first sandboxed exec and
 * reaped on its turn.finished (commands only run inside turns; a held request keeps
 * its turn alive because the tool call is blocked on the gate). A process backgrounded
 * from a turn (`foo &`) loses its proxy when the turn ends — the next turn gets a
 * fresh one.
 *
 * Opt-in for now (BOUGH_CLAWPATROL=1). The default flips to on once the approval UI can
 * clear held requests (until then a fail-closed default could wedge a turn with no way
 * to unblock it). With the flag off, no proxy ever starts and exec runs unrouted.
 */
import { caEnv, CertAuthority } from "./ca.ts";
import { ProxyServer } from "./proxy.ts";
import { createGate, type Gate } from "./gate.ts";
import { ExtensionHost, type ExtensionInfo } from "./extensions.ts";
import { loadConfig, resolveConfig, toPolicy } from "./config.ts";
import type { Db } from "../db/db.ts";
import type { Bus } from "../bus.ts";

/** Whether bough should run the egress proxy (opt-in — see module docs). */
export function clawpatrolEnabled(): boolean {
  return Deno.env.get("BOUGH_CLAWPATROL") === "1";
}

export interface GatewayStatus {
  enabled: boolean;
  running: boolean;
  /** Live per-session listeners (informational; each session gets its own port). */
  listeners: number;
  caPath: string;
}

/** Owns the CA + gate + per-session proxies for the server's lifetime. */
export class ClawpatrolGateway {
  #db: Db;
  #bus: Bus;
  #ca?: CertAuthority;
  #gate?: Gate;
  #extensions?: ExtensionHost;
  // Live listeners keyed by sessionId ("" = a caller with no session, e.g. tests).
  #proxies = new Map<string, ProxyServer>();
  // In-flight starts, so concurrent tool calls in one turn share one listener.
  #starting = new Map<string, Promise<ProxyServer>>();
  #running = false;
  #unsubscribe?: () => void;

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
      listeners: this.#proxies.size,
      caPath: this.#ca?.caCertPath ?? "",
    };
  }

  /** Reloadable programmable guards; see /net/extensions endpoints. */
  async reloadExtensions(): Promise<ExtensionInfo[]> {
    await this.#extensions?.load();
    return this.#extensions?.list() ?? [];
  }

  listExtensions(): ExtensionInfo[] {
    return this.#extensions?.list() ?? [];
  }

  /** Boot the CA, gate, and extensions when enabled; listeners start lazily per session. */
  async start(): Promise<void> {
    if (!clawpatrolEnabled()) return;
    this.#ca = CertAuthority.load();
    this.#extensions = new ExtensionHost(this.#db);
    await this.#extensions.load();
    this.#gate = createGate({
      db: this.#db,
      bus: this.#bus,
      policy: toPolicy(loadConfig()),
      // Branch policy: a session's own net_policies row, else the nearest
      // ancestor's, else the global rule set (config.ts resolveConfig).
      resolve: (sessionId) => toPolicy(resolveConfig(this.#db, sessionId).config),
      extensions: this.#extensions,
    });
    // When a session's turn ends: expire any holds it left parked (an interrupted
    // turn's command dies but its gate hold would otherwise pend forever), then
    // reap its listener; the next turn re-acquires one.
    this.#unsubscribe = this.#bus.subscribe((e) => {
      if (e.type === "turn.finished" && e.sessionId) {
        this.#gate?.expireHolds(e.sessionId, "expired — turn ended before approval");
        void this.release(e.sessionId);
      }
    });
    this.#running = true;
    console.log(`[clawpatrol] native egress gateway up (per-session listeners)`);
    console.log(`[clawpatrol] sandbox clients trust the MITM CA at ${this.#ca.caCertPath}`);
  }

  async stop(): Promise<void> {
    this.#running = false;
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
    const live = [...this.#proxies.values()];
    this.#proxies.clear();
    await Promise.all(live.map((p) => p.stop()));
  }

  /** The session's live proxy, starting one if needed. */
  #acquire(key: string): Promise<ProxyServer> {
    const live = this.#proxies.get(key);
    if (live) return Promise.resolve(live);
    let starting = this.#starting.get(key);
    if (!starting) {
      const gate = this.#gate!;
      const proxy = new ProxyServer({
        ca: this.#ca!,
        gate: (req, opts) => gate.gate(req, opts),
        sessionId: key || undefined,
      });
      starting = proxy.start().then(() => {
        this.#starting.delete(key);
        this.#proxies.set(key, proxy);
        return proxy;
      });
      this.#starting.set(key, starting);
    }
    return starting;
  }

  /** Stop and forget a session's listener (turn ended / session archived). */
  async release(sessionId: string): Promise<void> {
    await this.#starting.get(sessionId)?.catch(() => {});
    const proxy = this.#proxies.get(sessionId);
    if (!proxy) return;
    this.#proxies.delete(sessionId);
    await proxy.stop();
  }

  /**
   * Env for a sandboxed command: point its HTTP(S) client at ITS SESSION's proxy and
   * trust the MITM CA. NO_PROXY keeps loopback (bough's own server, and the proxy
   * itself) direct so requests don't loop. Empty when the gateway isn't running.
   */
  async envFor(sessionId?: string): Promise<Record<string, string>> {
    if (!this.#running || !this.#ca || !this.#gate) return {};
    const proxy = await this.#acquire(sessionId ?? "");
    const url = proxy.url;
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
export function clawpatrolEnv(sessionId?: string): Promise<Record<string, string>> {
  return active?.envFor(sessionId) ?? Promise.resolve({});
}
