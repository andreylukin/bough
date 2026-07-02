import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import net from "node:net";
import tls from "node:tls";
import http from "node:http";
import { CertAuthority } from "./ca.ts";
import { ProxyServer } from "./proxy.ts";
import type { Decision, Request as GateRequest } from "./policy.ts";

const allow = (reason = "ok"): Decision => ({
  verdict: "allow",
  reason,
  action: { service: "test", verb: "x", kind: "read" },
});
const deny = (reason = "no"): Decision => ({
  verdict: "deny",
  reason,
  action: { service: "test", verb: "x", kind: "write" },
});

async function makeCA(): Promise<CertAuthority> {
  const dir = await Deno.makeTempDir({ prefix: "bough-proxy-ca-" });
  return CertAuthority.load(dir);
}

/** A local plain-HTTP origin that echoes a fixed body; returns {port, hits}. */
function originHttp(bodyText: string) {
  const hits: http.IncomingMessage[] = [];
  const server = http.createServer((req, res) => {
    hits.push(req);
    res.writeHead(200, { "content-type": "text/plain" });
    res.end(bodyText);
  });
  const ready = new Promise<number>((r) =>
    server.listen(0, "127.0.0.1", () => r((server.address() as net.AddressInfo).port))
  );
  return { server, hits, ready };
}

/** Raw HTTP/1.1 exchange over a socket connected to the proxy (absolute-URI form). */
function rawExchange(port: number, requestLine: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const sock = net.connect(port, "127.0.0.1", () => sock.write(requestLine));
    let buf = "";
    sock.on("data", (d: Uint8Array) => (buf += new TextDecoder().decode(d)));
    sock.on("end", () => resolve(buf));
    sock.on("error", reject);
  });
}

Deno.test("plain-HTTP allow: forwards to origin and returns its body", async () => {
  const ca = await makeCA();
  const origin = originHttp("hello-from-origin");
  const oport = await origin.ready;
  const proxy = new ProxyServer({ ca, gate: () => Promise.resolve(allow()) });
  await proxy.start();
  try {
    const res = await rawExchange(
      proxy.port,
      `GET http://127.0.0.1:${oport}/data HTTP/1.1\r\nHost: 127.0.0.1:${oport}\r\nConnection: close\r\n\r\n`,
    );
    assertStringIncludes(res, "200");
    assertStringIncludes(res, "hello-from-origin");
    assertEquals(origin.hits.length, 1);
    assertEquals(origin.hits[0].url, "/data");
  } finally {
    await proxy.stop();
    origin.server.close();
  }
});

Deno.test("plain-HTTP deny: returns 403 and never contacts the origin", async () => {
  const ca = await makeCA();
  const origin = originHttp("should-not-see-this");
  const oport = await origin.ready;
  const proxy = new ProxyServer({ ca, gate: () => Promise.resolve(deny("write blocked")) });
  await proxy.start();
  try {
    const res = await rawExchange(
      proxy.port,
      `DELETE http://127.0.0.1:${oport}/x HTTP/1.1\r\nHost: 127.0.0.1:${oport}\r\nConnection: close\r\n\r\n`,
    );
    assertStringIncludes(res, "403");
    assertStringIncludes(res, "Blocked by Claw Patrol: write blocked");
    assertEquals(origin.hits.length, 0);
  } finally {
    await proxy.stop();
    origin.server.close();
  }
});

Deno.test("CONNECT MITM: terminates TLS, the gate sees the decrypted POST body, deny → 403 over TLS", async () => {
  const ca = await makeCA();
  const seen: GateRequest[] = [];
  const gate: (r: GateRequest) => Promise<Decision> = (r) => {
    seen.push(r);
    return Promise.resolve(deny("graphql mutation blocked"));
  };
  const proxy = new ProxyServer({ ca, gate });
  await proxy.start();

  const host = "api.example.test";
  const payload = '{"query":"mutation { doThing }"}';

  const responseText = await new Promise<string>((resolve, reject) => {
    // 1. open the CONNECT tunnel to the proxy
    const raw = net.connect(proxy.port, "127.0.0.1", () => {
      raw.write(`CONNECT ${host}:443 HTTP/1.1\r\nHost: ${host}:443\r\n\r\n`);
    });
    let established = false;
    raw.on("data", function onData(d: Uint8Array) {
      if (established) return;
      const s = new TextDecoder().decode(d);
      if (!s.includes("200")) return reject(new Error("tunnel not established: " + s));
      established = true;
      raw.removeListener("data", onData);
      // 2. TLS-handshake through the tunnel, trusting ONLY our MITM CA
      const tlsSock = tls.connect(
        { socket: raw, servername: host, ca: [ca.caCertPem] },
        () => {
          tlsSock.write(
            `POST /graphql HTTP/1.1\r\nHost: ${host}\r\ncontent-type: application/json\r\n` +
              `content-length: ${payload.length}\r\nConnection: close\r\n\r\n${payload}`,
          );
        },
      );
      let buf = "";
      tlsSock.on("data", (b: Uint8Array) => (buf += new TextDecoder().decode(b)));
      tlsSock.on("close", () => resolve(buf));
      tlsSock.on("error", reject);
    });
    raw.on("error", reject);
  });

  try {
    // The decrypted request was fully visible to the gate.
    assertEquals(seen.length, 1);
    assertEquals(seen[0].host, host);
    assertEquals(seen[0].method, "POST");
    assertEquals(seen[0].path, "/graphql");
    assertStringIncludes(new TextDecoder().decode(seen[0].body as Uint8Array), "mutation");
    // And the deny came back as a 403 over the terminated TLS.
    assertStringIncludes(responseText, "403");
    assertStringIncludes(responseText, "graphql mutation blocked");
  } finally {
    await proxy.stop();
  }
});
