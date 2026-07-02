/**
 * The local certificate authority Claw Patrol's MITM proxy uses to terminate TLS.
 *
 * A root CA is generated once and persisted (~/.bough/net/ca/); sandboxed clients
 * trust it via the CA-env exports (see proxy.ts / caEnv). For each intercepted host
 * the proxy needs a leaf cert whose SAN matches — we mint those on demand and sign
 * them with the CA. Following mitmproxy's optimisation, every leaf shares ONE
 * keypair (also persisted): only the certificate is per-host, so minting a new host
 * costs a cheap signature, not a fresh RSA keygen.
 *
 * forge is synchronous and CPU-bound; the one-time keygens block briefly on first
 * run, then everything is cache hits. Deno's node:tls terminates with the PEM pair
 * this module hands out (via SNICallback → tls.createSecureContext).
 */
import { join } from "node:path";
import { homedir } from "node:os";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import forge from "node-forge";

// forge ships no bundled types under npm: resolution; treat pki as structural.
// deno-lint-ignore no-explicit-any
const pki: any = forge.pki;
// deno-lint-ignore no-explicit-any
type Cert = any;
// deno-lint-ignore no-explicit-any
type Key = any;

/** A minted leaf as the PEM pair node:tls wants. */
export interface LeafPem {
  key: string;
  cert: string;
}

const CA_SUBJECT = [
  { name: "commonName", value: "bough Claw Patrol CA" },
  { name: "organizationName", value: "bough" },
];

function newSerial(): string {
  // Positive, unique-ish hex serial. forge wants a hex string; a leading "0"
  // keeps the high bit clear so the integer stays positive.
  return "0" + Math.floor(performance.now() * 1000).toString(16) + "1";
}

/** Self-signed root CA (5-year validity). */
function generateCA(): { cert: Cert; key: Key } {
  const keys = pki.rsa.generateKeyPair(2048);
  const cert = pki.createCertificate();
  cert.publicKey = keys.publicKey;
  cert.serialNumber = "01";
  cert.validity.notBefore = new Date();
  cert.validity.notAfter = new Date();
  cert.validity.notAfter.setFullYear(cert.validity.notBefore.getFullYear() + 5);
  cert.setSubject(CA_SUBJECT);
  cert.setIssuer(CA_SUBJECT);
  cert.setExtensions([
    { name: "basicConstraints", cA: true },
    { name: "keyUsage", keyCertSign: true, cRLSign: true, digitalSignature: true },
  ]);
  cert.sign(keys.privateKey, forge.md.sha256.create());
  return { cert, key: keys.privateKey };
}

/** The default net dir (BOUGH_NET_DIR overrides in tests); CA lives in its `ca/` subdir. */
export function caDir(): string {
  const base = Deno.env.get("BOUGH_NET_DIR") ?? join(homedir(), ".bough", "net");
  const dir = join(base, "ca");
  mkdirSync(dir, { recursive: true });
  return dir;
}

/**
 * The MITM certificate authority: a persisted root CA plus a shared leaf keypair,
 * minting and caching one leaf certificate per intercepted host.
 */
export class CertAuthority {
  #caCert: Cert;
  #caKey: Key;
  #leafPubKey: Key;
  #leafPrivPem: string;
  #leaves = new Map<string, LeafPem>();

  readonly caCertPem: string;
  readonly caCertPath: string;

  private constructor(opts: {
    caCert: Cert;
    caKey: Key;
    leafPubKey: Key;
    leafPrivPem: string;
    caCertPath: string;
  }) {
    this.#caCert = opts.caCert;
    this.#caKey = opts.caKey;
    this.#leafPubKey = opts.leafPubKey;
    this.#leafPrivPem = opts.leafPrivPem;
    this.caCertPem = pki.certificateToPem(opts.caCert);
    this.caCertPath = opts.caCertPath;
  }

  /** Load the CA + shared leaf key from `dir`, generating and persisting them on first run. */
  static load(dir = caDir()): CertAuthority {
    const caCertPath = join(dir, "ca.crt");
    const caKeyPath = join(dir, "ca.key");
    const leafKeyPath = join(dir, "leaf.key");

    let caCert: Cert;
    let caKey: Key;
    if (existsSync(caCertPath) && existsSync(caKeyPath)) {
      caCert = pki.certificateFromPem(readFileSync(caCertPath, "utf8"));
      caKey = pki.privateKeyFromPem(readFileSync(caKeyPath, "utf8"));
    } else {
      const ca = generateCA();
      caCert = ca.cert;
      caKey = ca.key;
      writeFileSync(caCertPath, pki.certificateToPem(caCert));
      writeFileSync(caKeyPath, pki.privateKeyToPem(caKey), { mode: 0o600 });
    }

    let leafPrivPem: string;
    let leafPubKey: Key;
    if (existsSync(leafKeyPath)) {
      leafPrivPem = readFileSync(leafKeyPath, "utf8");
      const priv = pki.privateKeyFromPem(leafPrivPem);
      leafPubKey = pki.setRsaPublicKey(priv.n, priv.e);
    } else {
      const keys = pki.rsa.generateKeyPair(2048);
      leafPrivPem = pki.privateKeyToPem(keys.privateKey);
      leafPubKey = keys.publicKey;
      writeFileSync(leafKeyPath, leafPrivPem, { mode: 0o600 });
    }

    return new CertAuthority({ caCert, caKey, leafPubKey, leafPrivPem, caCertPath });
  }

  /** The CA-signed leaf PEM pair for `host`, minted once and cached. */
  leafFor(host: string): LeafPem {
    const cached = this.#leaves.get(host);
    if (cached) return cached;

    const cert = pki.createCertificate();
    cert.publicKey = this.#leafPubKey;
    cert.serialNumber = newSerial();
    cert.validity.notBefore = new Date(Date.now() - 60_000); // small backdate for clock skew
    cert.validity.notAfter = new Date();
    cert.validity.notAfter.setFullYear(cert.validity.notBefore.getFullYear() + 1);
    cert.setSubject([{ name: "commonName", value: host }]);
    cert.setIssuer(this.#caCert.subject.attributes);
    cert.setExtensions([
      { name: "basicConstraints", cA: false },
      { name: "keyUsage", digitalSignature: true, keyEncipherment: true },
      { name: "extKeyUsage", serverAuth: true },
      { name: "subjectAltName", altNames: [sanFor(host)] },
    ]);
    cert.sign(this.#caKey, forge.md.sha256.create());

    const pem: LeafPem = { key: this.#leafPrivPem, cert: pki.certificateToPem(cert) };
    this.#leaves.set(host, pem);
    return pem;
  }
}

/** A subjectAltName entry: type 7 (IP) for literal addresses, type 2 (DNS) otherwise. */
function sanFor(host: string): { type: number; value?: string; ip?: string } {
  return /^\d+\.\d+\.\d+\.\d+$/.test(host) ? { type: 7, ip: host } : { type: 2, value: host };
}

/**
 * TLS-trust environment for sandboxed commands so their HTTPS clients trust the MITM
 * CA. Keyed to the CA cert on disk; every mainstream client reads one of these. Go
 * binaries ignore all of them (they use the system trust store) — those fall back to
 * host-level gating without body inspection.
 */
export function caEnv(caCertPath: string): Record<string, string> {
  return {
    SSL_CERT_FILE: caCertPath,
    NODE_EXTRA_CA_CERTS: caCertPath,
    REQUESTS_CA_BUNDLE: caCertPath,
    CURL_CA_BUNDLE: caCertPath,
    GIT_SSL_CAINFO: caCertPath,
    DENO_CERT: caCertPath,
    PIP_CERT: caCertPath,
    AWS_CA_BUNDLE: caCertPath,
  };
}
