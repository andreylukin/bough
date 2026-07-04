/**
 * Claw Patrol's native egress proxy — the on-the-wire enforcement bough runs in
 * place of the retired Go `clawpatrol gateway`. Sandboxed shell commands are pointed
 * at it via HTTPS_PROXY + the CA-trust env (see ca.ts `caEnv`); Seatbelt still
 * confines the filesystem.
 *
 * Shape (proven in the Phase-0 spike, relies on Deno 2.9 node:tls):
 *   forward server ──CONNECT host:443──▶ ack tunnel, hand raw socket to the TLS
 *   terminator ──SNICallback mints a CA-signed leaf──▶ decrypted HTTP server reads
 *   the plaintext request ──▶ gate(req) ──▶ allow: re-originate TLS to the origin
 *   (+ inject credentials) and stream back; deny: synthesize 403.
 *
 * The gate is injected (GateFn): it owns classify → decide → hold-and-ask and only
 * ever resolves to a final allow/deny, so this module never sees "hold" — a held
 * request simply keeps its socket parked until a human resolves it. That keeps the
 * proxy a pure transport and the policy/approval logic in gate.ts.
 *
 * MITM-everything: we terminate TLS for every CONNECT and let the gate deny by host
 * OR action, rather than pre-filtering at CONNECT — a clear "403 blocked by Claw
 * Patrol" beats an opaque TLS/connection error, and hold-on-first-seen-host works
 * because the decrypted request carries both host and action.
 */
import http from "node:http";
import https from "node:https";
import net from "node:net";
import tls from "node:tls";
import type { CertAuthority } from "./ca.ts";
import type { Decision, Request as GateRequest } from "./policy.ts";

/** classify → decide → hold-and-ask; resolves to a FINAL allow/deny (never hold). */
export type GateFn = (req: GateRequest, opts: { sessionId?: string }) => Promise<Decision>;

/** A header the proxy stamps onto allowed requests for a host — the token never enters the sandbox. */
export interface CredentialRule {
  /** exact host or "*.suffix". */
  host: string;
  header: string; // e.g. "authorization"
  value: string; // e.g. "Bearer ghp_…"
}

export interface ProxyOptions {
  ca: CertAuthority;
  gate: GateFn;
  host?: string; // bind address, default 127.0.0.1
  port?: number; // default 0 (ephemeral)
  credentials?: CredentialRule[];
  /** Tag events with the session that owns this egress (single-session servers). */
  sessionId?: string;
}

const HOP_BY_HOP = new Set([
  "proxy-connection",
  "connection",
  "keep-alive",
  "transfer-encoding",
  "te",
  "trailer",
  "upgrade",
]);

function stripPort(hostHeader = ""): string {
  const h = hostHeader.trim();
  // IPv6 literals are bracketed; a bare host may carry :port.
  if (h.startsWith("[")) return h.slice(1, h.indexOf("]"));
  const colon = h.lastIndexOf(":");
  return colon > 0 ? h.slice(0, colon) : h;
}

function hostMatches(host: string, pattern: string): boolean {
  return host === pattern || (pattern.startsWith("*.") && host.endsWith(pattern.slice(1)));
}

/** Coerce node's string|string[] header bag to the flat map policy.ts expects. */
function flatHeaders(h: http.IncomingHttpHeaders): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(h)) {
    if (v === undefined) continue;
    out[k] = Array.isArray(v) ? v.join(", ") : v;
  }
  return out;
}

function readBody(req: http.IncomingMessage): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    const chunks: Uint8Array[] = [];
    req.on("data", (c: Uint8Array) => chunks.push(c));
    req.on("end", () => {
      const total = chunks.reduce((n, c) => n + c.length, 0);
      const buf = new Uint8Array(total);
      let off = 0;
      for (const c of chunks) {
        buf.set(c, off);
        off += c.length;
      }
      resolve(buf);
    });
    req.on("error", reject);
  });
}

export class ProxyServer {
  #opts: ProxyOptions;
  #proxy: http.Server; // CONNECT + plain-HTTP forward server
  #mitm: tls.Server; // SNI-driven TLS terminator for CONNECT tunnels
  #mitmHttp: http.Server; // reads the decrypted request off terminated tunnels
  #port = 0;

  constructor(opts: ProxyOptions) {
    this.#opts = opts;

    // The decrypted-HTTP handler for MITM'd tunnels. `req.socket` is the TLS socket,
    // whose SNI servername is the real origin host we terminated for.
    this.#mitmHttp = http.createServer((creq, cres) => {
      // deno-lint-ignore no-explicit-any
      const servername = (creq.socket as any).servername as string | undefined;
      const host = servername || stripPort(creq.headers.host);
      this.#handle(creq, cres, host, true);
    });

    this.#mitm = tls.createServer({
      // A default context so the handshake can start before SNI resolves; per-host
      // leaves are chosen by SNICallback. Both share the CA's leaf keypair.
      ...opts.ca.leafFor("localhost"),
      SNICallback: (servername: string, cb: (e: Error | null, ctx?: tls.SecureContext) => void) => {
        const leaf = opts.ca.leafFor(servername);
        cb(null, tls.createSecureContext({ key: leaf.key, cert: leaf.cert }));
      },
    });
    this.#mitm.on(
      "secureConnection",
      (sock: unknown) => this.#mitmHttp.emit("connection", sock),
    );
    this.#mitm.on("tlsClientError", () => {}); // client bailed mid-handshake; ignore

    this.#proxy = http.createServer((creq, cres) => {
      // Plain-HTTP forward proxy: the request line is an absolute URI.
      const url = safeUrl(creq.url);
      this.#handle(creq, cres, url?.hostname ?? stripPort(creq.headers.host), false);
    });
    this.#proxy.on("connect", (_req: http.IncomingMessage, socket: net.Socket) => {
      socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      // Hand the raw tunnel to the TLS terminator; it emits secureConnection → mitmHttp.
      this.#mitm.emit("connection", socket);
    });
    this.#proxy.on("clientError", (_e: Error, socket: net.Socket) => {
      try {
        socket.destroy();
      } catch { /* already gone */ }
    });
  }

  /** Gate one request, then forward it (allow) or reject it (deny). `secure` picks http/https origin. */
  async #handle(
    creq: http.IncomingMessage,
    cres: http.ServerResponse,
    host: string,
    secure: boolean,
  ): Promise<void> {
    // A client that vanishes mid-exchange (timeout, ^C) must never take the proxy
    // down with an unhandled 'error' from its dead socket.
    cres.on("error", () => {});
    let body: Uint8Array;
    try {
      body = await readBody(creq);
    } catch {
      cres.destroy();
      return;
    }

    const path = safeUrl(creq.url)?.pathname ?? creq.url ?? "/";
    const gateReq: GateRequest = {
      host,
      method: creq.method ?? "GET",
      path: secure ? (creq.url ?? "/") : path,
      headers: flatHeaders(creq.headers),
      body,
    };

    let decision: Decision;
    try {
      decision = await this.#opts.gate(gateReq, { sessionId: this.#opts.sessionId });
    } catch (e) {
      this.#deny(cres, `gate error: ${(e as Error).message}`);
      return;
    }

    if (decision.verdict !== "allow") {
      this.#deny(cres, decision.reason);
      return;
    }
    // A hold can resolve long after the requester gave up (curl --max-time while the
    // approval card sat unanswered). Forwarding then would fire the side effect into
    // the void and write the response to a dead socket — skip; the approval released
    // THIS request, and this request no longer has a client. (Check the response
    // side only: creq.destroyed is routinely true once the body was fully read.)
    if (cres.destroyed || cres.writableEnded) return;
    this.#forward(creq, cres, host, secure, body);
  }

  /** Re-originate the request to the real host and stream the response back. */
  #forward(
    creq: http.IncomingMessage,
    cres: http.ServerResponse,
    host: string,
    secure: boolean,
    body: Uint8Array,
  ): void {
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(flatHeaders(creq.headers))) {
      if (!HOP_BY_HOP.has(k.toLowerCase())) headers[k] = v;
    }
    // Stamp credentials for this host — the token never entered the sandbox.
    for (const cred of this.#opts.credentials ?? []) {
      if (hostMatches(host, cred.host)) headers[cred.header] = cred.value;
    }

    const u = safeUrl(creq.url);
    const path = u?.pathname ?? creq.url ?? "/";
    const search = u?.search ?? "";
    const client = secure ? https : http;
    const oreq = client.request(
      {
        host,
        servername: secure ? host : undefined,
        port: secure ? 443 : (Number(u?.port) || 80),
        method: creq.method,
        path: secure ? (creq.url ?? "/") : path + search,
        headers,
      },
      (ores: http.IncomingMessage) => {
        if (cres.destroyed) {
          ores.destroy();
          return;
        }
        cres.writeHead(ores.statusCode ?? 502, ores.headers);
        ores.pipe(cres);
      },
    );
    oreq.on("error", (e: Error) => {
      if (!cres.headersSent) cres.writeHead(502, { "content-type": "text/plain" });
      cres.end(`Claw Patrol: upstream error contacting ${host}: ${e.message}`);
    });
    if (body.length) oreq.write(body);
    oreq.end();
  }

  #deny(cres: http.ServerResponse, reason: string): void {
    if (!cres.headersSent) {
      cres.writeHead(403, { "content-type": "text/plain", "x-clawpatrol": "denied" });
    }
    cres.end(`Blocked by Claw Patrol: ${reason}\n`);
  }

  async start(): Promise<void> {
    const host = this.#opts.host ?? "127.0.0.1";
    await new Promise<void>((resolve, reject) => {
      this.#proxy.once("error", reject);
      this.#proxy.listen(this.#opts.port ?? 0, host, () => {
        this.#port = (this.#proxy.address() as net.AddressInfo).port;
        resolve();
      });
    });
  }

  get port(): number {
    return this.#port;
  }

  /** The proxy URL sandboxed clients point HTTPS_PROXY/HTTP_PROXY at. */
  get url(): string {
    return `http://${this.#opts.host ?? "127.0.0.1"}:${this.#port}`;
  }

  async stop(): Promise<void> {
    await Promise.all([
      new Promise<void>((r) => this.#proxy.close(() => r())),
      new Promise<void>((r) => this.#mitm.close(() => r())),
      new Promise<void>((r) => this.#mitmHttp.close(() => r())),
    ]);
  }
}

function safeUrl(u: string | undefined): URL | undefined {
  if (!u) return undefined;
  try {
    return new URL(u);
  } catch {
    return undefined;
  }
}
