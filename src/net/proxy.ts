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
import { hostMatches } from "./policy.ts";
import type { Decision, Request as GateRequest } from "./policy.ts";

/** classify → decide → hold-and-ask; resolves to a FINAL allow/deny (never hold). */
export type GateFn = (req: GateRequest, opts: { sessionId?: string }) => Promise<Decision>;

/** A header the proxy stamps onto allowed requests for a host — the token never enters the sandbox. */
export interface CredentialRule {
  /** exact host or "*.suffix". */
  host: string;
  header: string; // e.g. "authorization"
  /**
   * The header value, or a provider for one that must be minted/refreshed (an EKS
   * exec token — see execcred.ts). A provider that throws fails the request with a
   * 502 naming the mint error, so an expired SSO session surfaces in the output
   * instead of as a mystery 401 from the origin.
   */
  value: string | (() => Promise<string>);
}

export interface ProxyOptions {
  ca: CertAuthority;
  gate: GateFn;
  host?: string; // bind address, default 127.0.0.1
  port?: number; // default 0 (ephemeral)
  credentials?: CredentialRule[];
  /** Tag events with the session that owns this egress (single-session servers). */
  sessionId?: string;
  /**
   * Extra CA (PEM) to trust when RE-ORIGINATING to a given host, keyed by exact host.
   * Needed for upstreams whose serving cert is signed by a PRIVATE CA the system
   * store lacks — a k8s API server (EKS uses the cluster CA, not a public root). AWS
   * and most APIs use public roots and need no entry. Without a match, default
   * (public) trust applies; we never disable verification.
   */
  upstreamCa?: Map<string, string>;
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
      const sock = creq.socket as any;
      const servername = sock.servername as string | undefined;
      const host = servername || stripPort(creq.headers.host);
      // The CONNECT target port, stashed on the raw socket before TLS termination
      // (below), so MITM'd HTTPS re-originates to the real port — 443 for EKS, but
      // 6443 for self-managed k8s, etc. Falls back to 443 if the link is missing.
      const port = (sock._parent?.__cpTargetPort as number | undefined) ?? 443;
      this.#handle(creq, cres, host, true, port);
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
    this.#proxy.on("connect", (req: http.IncomingMessage, socket: net.Socket) => {
      // Remember the CONNECT target port so #forward can re-originate to it (the SNI
      // host we terminate for carries no port). Read back via the TLS socket's parent.
      const port = Number(req.url?.split(":")[1]);
      if (Number.isFinite(port)) {
        // deno-lint-ignore no-explicit-any
        (socket as any).__cpTargetPort = port;
      }
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
    port?: number,
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
    await this.#forward(creq, cres, host, secure, body, port);
  }

  /** Re-originate the request to the real host and stream the response back. */
  async #forward(
    creq: http.IncomingMessage,
    cres: http.ServerResponse,
    host: string,
    secure: boolean,
    body: Uint8Array,
    port?: number,
  ): Promise<void> {
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(flatHeaders(creq.headers))) {
      if (!HOP_BY_HOP.has(k.toLowerCase())) headers[k] = v;
    }
    // Stamp credentials for this host — the token never entered the sandbox.
    for (const cred of this.#opts.credentials ?? []) {
      if (!hostMatches(host, [cred.host])) continue;
      try {
        headers[cred.header] = typeof cred.value === "string" ? cred.value : await cred.value();
      } catch (e) {
        if (!cres.headersSent) cres.writeHead(502, { "content-type": "text/plain" });
        cres.end(`Claw Patrol: credential mint failed for ${host}: ${(e as Error).message}\n`);
        return;
      }
    }

    const u = safeUrl(creq.url);
    const path = u?.pathname ?? creq.url ?? "/";
    const search = u?.search ?? "";
    const client = secure ? https : http;
    // Private-CA upstreams (a k8s API server) need their cluster CA added to trust;
    // everything else keeps default public-root verification (never disabled).
    const extraCa = secure ? this.#opts.upstreamCa?.get(host) : undefined;
    const oreq = client.request(
      {
        host,
        servername: secure ? host : undefined,
        port: secure ? (port ?? 443) : (Number(u?.port) || 80),
        method: creq.method,
        path: secure ? (creq.url ?? "/") : path + search,
        headers,
        ...(extraCa ? { ca: [tls.rootCertificates.join("\n"), extraCa].join("\n") } : {}),
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
