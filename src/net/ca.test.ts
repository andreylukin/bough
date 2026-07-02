import { assert, assertEquals } from "jsr:@std/assert@1";
import { join } from "node:path";
import { CertAuthority } from "./ca.ts";
import tls from "node:tls";
import net from "node:net";
import forge from "node-forge";

// deno-lint-ignore no-explicit-any
const pki: any = forge.pki;

async function withCaDir(fn: (dir: string) => Promise<void> | void) {
  const dir = await Deno.makeTempDir({ prefix: "bough-ca-test-" });
  try {
    await fn(dir);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
}

Deno.test("CertAuthority persists the CA + shared leaf key across loads", async () => {
  await withCaDir((dir) => {
    const a = CertAuthority.load(dir);
    assert(existsSyncSafe(join(dir, "ca.crt")));
    assert(existsSyncSafe(join(dir, "ca.key")));
    assert(existsSyncSafe(join(dir, "leaf.key")));
    // Reloading reuses the same CA cert (not a fresh keygen).
    const b = CertAuthority.load(dir);
    assertEquals(a.caCertPem, b.caCertPem);
  });
});

Deno.test("leafFor is memoized per host and SAN-matches", async () => {
  await withCaDir((dir) => {
    const ca = CertAuthority.load(dir);
    const first = ca.leafFor("api.github.com");
    const again = ca.leafFor("api.github.com");
    assertEquals(first.cert, again.cert); // cached, identical

    const cert = pki.certificateFromPem(first.cert);
    const cn = cert.subject.getField("CN");
    assertEquals(cn.value, "api.github.com");
    const san = cert.getExtension("subjectAltName");
    assert(san.altNames.some((n: { value?: string }) => n.value === "api.github.com"));
  });
});

Deno.test("a minted leaf terminates real TLS and the CA validates the chain", async () => {
  await withCaDir(async (dir) => {
    const ca = CertAuthority.load(dir);
    const leaf = ca.leafFor("localhost");

    // Server terminates with the minted leaf.
    const server = tls.createServer({ key: leaf.key, cert: leaf.cert }, (sock: net.Socket) => {
      sock.on("data", () => sock.end("ok"));
    });
    const port: number = await new Promise((res) =>
      server.listen(0, "127.0.0.1", () => res((server.address() as net.AddressInfo).port))
    );

    // Client trusts ONLY our CA — a successful handshake proves the chain is valid.
    const body = await new Promise<string>((resolve, reject) => {
      const c = tls.connect(
        { host: "127.0.0.1", port, servername: "localhost", ca: [ca.caCertPem] },
        () => c.write("hi"),
      );
      let buf = "";
      c.on("data", (d: Uint8Array) => (buf += new TextDecoder().decode(d)));
      c.on("end", () => resolve(buf));
      c.on("error", reject);
    });
    assertEquals(body, "ok");
    server.close();
  });
});

function existsSyncSafe(p: string): boolean {
  try {
    Deno.statSync(p);
    return true;
  } catch {
    return false;
  }
}
