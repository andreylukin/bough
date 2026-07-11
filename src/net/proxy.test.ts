import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
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
    server.listen(0, "localhost", () => r((server.address() as net.AddressInfo).port))
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

/** CONNECT host:port through the proxy, TLS-trust `downstreamCaPem`, GET / → body. */
function connectMitmGet(
  proxyPort: number,
  host: string,
  targetPort: number,
  downstreamCaPem: string,
): Promise<string> {
  return new Promise((resolve, reject) => {
    const raw = net.connect(proxyPort, "127.0.0.1", () => {
      raw.write(`CONNECT ${host}:${targetPort} HTTP/1.1\r\nHost: ${host}:${targetPort}\r\n\r\n`);
    });
    let acked = false;
    raw.on("data", (d: Uint8Array) => {
      if (acked) return;
      acked = true;
      const t = tls.connect(
        { socket: raw, servername: host, ca: [downstreamCaPem] },
        () => t.write(`GET / HTTP/1.1\r\nHost: ${host}\r\nConnection: close\r\n\r\n`),
      );
      let buf = "";
      t.on("data", (b: Uint8Array) => (buf += new TextDecoder().decode(b)));
      t.on("close", () => resolve(buf));
      t.on("error", reject);
      void d;
    });
    raw.on("error", reject);
  });
}

/** A TLS "EKS API server" using `serverCa`'s leaf for 127.0.0.1; replies with a fixed body. */
function fakeEks(serverCa: CertAuthority) {
  const leaf = serverCa.leafFor("localhost");
  const server = tls.createServer({ key: leaf.key, cert: leaf.cert }, (sock: tls.TLSSocket) => {
    let got = false;
    sock.on("data", () => {
      if (got) return;
      got = true;
      sock.write(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 9\r\nConnection: close\r\n\r\nfrom-eks!",
      );
    });
    sock.on("error", () => {});
  });
  const ready = new Promise<number>((r) =>
    server.listen(0, "localhost", () => r((server.address() as net.AddressInfo).port))
  );
  return { server, ready };
}

Deno.test("MITM upstream CA: re-originates to a private-CA server (fake EKS) on its own port", async () => {
  const ca = await makeCA(); // bough's MITM CA — trusted DOWNSTREAM by the client
  const clusterCa = await makeCA(); // the cluster's private CA — server is signed by it
  const eks = fakeEks(clusterCa);
  const eport = await eks.ready;
  const proxy = new ProxyServer({
    ca,
    gate: () => Promise.resolve(allow()),
    upstreamCa: new Map([["localhost", clusterCa.caCertPem]]), // trust the cluster UPSTREAM
  });
  await proxy.start();
  try {
    const body = await connectMitmGet(proxy.port, "localhost", eport, ca.caCertPem);
    assertStringIncludes(body, "200 OK");
    assertStringIncludes(body, "from-eks!"); // proxy trusted the private cluster cert
  } finally {
    await proxy.stop();
    eks.server.close();
  }
});

Deno.test("MITM upstream CA: without the cluster CA, re-origination fails closed (502)", async () => {
  const ca = await makeCA();
  const clusterCa = await makeCA();
  const eks = fakeEks(clusterCa);
  const eport = await eks.ready;
  const proxy = new ProxyServer({ ca, gate: () => Promise.resolve(allow()) }); // no upstreamCa
  await proxy.start();
  try {
    const body = await connectMitmGet(proxy.port, "localhost", eport, ca.caCertPem);
    assertStringIncludes(body, "502"); // proxy won't trust the unknown cluster cert
    assertStringIncludes(body, "Claw Patrol: upstream error");
  } finally {
    await proxy.stop();
    eks.server.close();
  }
});

Deno.test("credentials: a provider value is minted host-side and stamped; origin sees the header", async () => {
  const ca = await makeCA();
  const origin = originHttp("ok");
  const oport = await origin.ready;
  let mints = 0;
  const proxy = new ProxyServer({
    ca,
    gate: () => Promise.resolve(allow()),
    credentials: [{
      host: "127.0.0.1",
      header: "authorization",
      value: () => Promise.resolve(`Bearer minted-${++mints}`),
    }],
  });
  await proxy.start();
  try {
    for (let i = 0; i < 2; i++) {
      await rawExchange(
        proxy.port,
        `GET http://127.0.0.1:${oport}/ HTTP/1.1\r\nHost: 127.0.0.1:${oport}\r\nConnection: close\r\n\r\n`,
      );
    }
    assertEquals(origin.hits.length, 2);
    // the provider runs per request (caching is the provider's job — see execcred.ts)
    assertEquals(origin.hits[0].headers["authorization"], "Bearer minted-1");
    assertEquals(origin.hits[1].headers["authorization"], "Bearer minted-2");
  } finally {
    await proxy.stop();
    origin.server.close();
  }
});

Deno.test("credentials: multiple rules for one host compose (authorization + impersonate-user)", async () => {
  const ca = await makeCA();
  const origin = originHttp("ok");
  const oport = await origin.ready;
  const proxy = new ProxyServer({
    ca,
    gate: () => Promise.resolve(allow()),
    // The kube impersonation shape: an exec-minted Authorization plus a constant
    // Impersonate-User, both matching the same host — the stamping loop sets both.
    credentials: [
      { host: "127.0.0.1", header: "authorization", value: () => Promise.resolve("Bearer admin") },
      { host: "127.0.0.1", header: "impersonate-user", value: "bough" },
    ],
  });
  await proxy.start();
  try {
    await rawExchange(
      proxy.port,
      `GET http://127.0.0.1:${oport}/ HTTP/1.1\r\nHost: 127.0.0.1:${oport}\r\nConnection: close\r\n\r\n`,
    );
    assertEquals(origin.hits[0].headers["authorization"], "Bearer admin");
    assertEquals(origin.hits[0].headers["impersonate-user"], "bough");
  } finally {
    await proxy.stop();
    origin.server.close();
  }
});

Deno.test("credentials: a provider that throws fails the request with a 502 naming the mint error", async () => {
  const ca = await makeCA();
  const origin = originHttp("should-not-see-this");
  const oport = await origin.ready;
  const proxy = new ProxyServer({
    ca,
    gate: () => Promise.resolve(allow()),
    credentials: [{
      host: "127.0.0.1",
      header: "authorization",
      value: () => Promise.reject(new Error("aws sso session expired")),
    }],
  });
  await proxy.start();
  try {
    const res = await rawExchange(
      proxy.port,
      `GET http://127.0.0.1:${oport}/ HTTP/1.1\r\nHost: 127.0.0.1:${oport}\r\nConnection: close\r\n\r\n`,
    );
    assertStringIncludes(res, "502");
    assertStringIncludes(res, "credential mint failed");
    assertStringIncludes(res, "aws sso session expired");
    assertEquals(origin.hits.length, 0);
  } finally {
    await proxy.stop();
    origin.server.close();
  }
});
