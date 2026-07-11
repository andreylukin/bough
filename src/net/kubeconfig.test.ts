import { assertEquals, assertStringIncludes, assertThrows } from "jsr:@std/assert@1";
import { parse } from "@std/yaml";
import { kubeconfigPath, rewriteKubeconfig } from "./kubeconfig.ts";

const BOUGH_CA = "-----BEGIN CERTIFICATE-----\nBOUGHFAKE\n-----END CERTIFICATE-----\n";
const REAL_CA = "-----BEGIN CERTIFICATE-----\nCLUSTERROOT\n-----END CERTIFICATE-----\n";

function eksConfig(): string {
  return [
    "apiVersion: v1",
    "kind: Config",
    "clusters:",
    "- name: dev",
    "  cluster:",
    "    server: https://EABF96.gr7.us-east-2.eks.amazonaws.com",
    `    certificate-authority-data: ${btoa(REAL_CA)}`,
    "users:",
    "- name: dev",
    "  user:",
    "    exec:",
    "      apiVersion: client.authentication.k8s.io/v1beta1",
    "      command: aws",
    "      args: [eks, get-token, --cluster-name, dev]",
    "contexts:",
    "- name: dev",
    "  context: { cluster: dev, user: dev }",
    "current-context: dev",
    "",
  ].join("\n");
}

Deno.test("rewrite: cluster CA becomes bough's; original CA + host returned for upstream", () => {
  const { rewritten, clusters, clientCertUsers } = rewriteKubeconfig(eksConfig(), BOUGH_CA);

  // downstream: kubectl now trusts bough's CA
  const doc = parse(rewritten) as { clusters: { cluster: Record<string, string> }[] };
  assertEquals(atob(doc.clusters[0].cluster["certificate-authority-data"]), BOUGH_CA);
  assertEquals("certificate-authority" in doc.clusters[0].cluster, false);

  // upstream: the proxy learns the real cluster CA, keyed by (lowercased) host
  assertEquals(clusters, [{ host: "eabf96.gr7.us-east-2.eks.amazonaws.com", caPem: REAL_CA }]);
  assertEquals(clientCertUsers, []);
});

Deno.test("rewrite: exec auth is lifted out — stripped from the sandbox copy, keyed to its cluster host", () => {
  const { rewritten, execCreds } = rewriteKubeconfig(eksConfig(), BOUGH_CA);

  // the sandbox copy carries no exec block (the plugin couldn't run in-sandbox anyway:
  // it reads ~/.aws, which the seatbelt denies) — the host mints, the proxy stamps
  assertEquals(rewritten.includes("exec:"), false);
  assertEquals(rewritten.includes("get-token"), false);
  // the host learns what to run, keyed by cluster host via the context pairing
  assertEquals(execCreds, [{
    host: "eabf96.gr7.us-east-2.eks.amazonaws.com",
    command: "aws",
    args: ["eks", "get-token", "--cluster-name", "dev"],
    env: {},
  }]);
});

Deno.test("rewrite: exec env flattens to a map; static bearer is lifted host-side, stripped from the copy", () => {
  const cfg = [
    "clusters:",
    "- name: a",
    "  cluster: { server: 'https://a.example.com' }",
    "- name: b",
    "  cluster: { server: 'https://b.example.com' }",
    "users:",
    "- name: a",
    "  user:",
    "    exec:",
    "      command: aws",
    "      args: [eks, get-token, --cluster-name, a]",
    "      env:",
    "      - name: AWS_PROFILE",
    "        value: dev",
    "- name: b",
    "  user: { token: static-token }",
    "contexts:",
    "- name: a",
    "  context: { cluster: a, user: a }",
    "- name: b",
    "  context: { cluster: b, user: b }",
  ].join("\n");
  const { rewritten, execCreds, tokenCreds } = rewriteKubeconfig(cfg, BOUGH_CA);
  assertEquals(execCreds, [{
    host: "a.example.com",
    command: "aws",
    args: ["eks", "get-token", "--cluster-name", "a"],
    env: { AWS_PROFILE: "dev" },
  }]);
  // the static bearer is lifted to a host-side cred, keyed to its cluster host...
  assertEquals(tokenCreds, [{ host: "b.example.com", token: "static-token" }]);
  // ...and scrubbed from the sandbox copy (proxy injects it host-side instead)
  assertEquals(rewritten.includes("static-token"), false);
});

Deno.test("rewrite: tokenFile is read host-side, lifted, and stripped from the copy", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-kc-tf-" });
  try {
    await Deno.writeTextFile(`${dir}/tok`, "file-token\n");
    const cfg = [
      "clusters:",
      "- name: c",
      "  cluster: { server: 'https://c.example.com' }",
      "users:",
      "- name: c",
      "  user: { tokenFile: tok }",
      "contexts:",
      "- name: c",
      "  context: { cluster: c, user: c }",
    ].join("\n");
    const { rewritten, tokenCreds } = rewriteKubeconfig(cfg, BOUGH_CA, dir);
    assertEquals(tokenCreds, [{ host: "c.example.com", token: "file-token" }]); // trimmed
    assertEquals(rewritten.includes("tokenFile"), false);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("rewrite: certificate-authority FILE ref is read, then replaced by inline bough data", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-kc-" });
  try {
    await Deno.writeTextFile(`${dir}/ca.crt`, REAL_CA);
    const cfg = [
      "clusters:",
      "- name: c",
      "  cluster:",
      "    server: https://api.internal:6443",
      "    certificate-authority: ca.crt", // relative to baseDir
    ].join("\n");
    const { rewritten, clusters } = rewriteKubeconfig(cfg, BOUGH_CA, dir);
    assertEquals(clusters[0], { host: "api.internal:6443", caPem: REAL_CA });
    const doc = parse(rewritten) as { clusters: { cluster: Record<string, string> }[] };
    assertEquals(atob(doc.clusters[0].cluster["certificate-authority-data"]), BOUGH_CA);
    assertEquals("certificate-authority" in doc.clusters[0].cluster, false);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("rewrite: client-cert users are flagged (they break under MITM)", () => {
  const cfg = [
    "clusters:",
    "- name: c",
    "  cluster: { server: https://api:6443, certificate-authority-data: " + btoa(REAL_CA) + " }",
    "users:",
    "- name: mtls",
    "  user: { client-certificate-data: Zm9v, client-key-data: YmFy }",
    "- name: tok",
    "  user: { token: abc }",
  ].join("\n");
  const { clientCertUsers } = rewriteKubeconfig(cfg, BOUGH_CA);
  assertEquals(clientCertUsers, ["mtls"]);
});

Deno.test("rewrite: a cluster with no CA still gets bough's; host recorded with no caPem", () => {
  const cfg = "clusters:\n- name: c\n  cluster:\n    server: https://plain:6443\n";
  const { clusters, rewritten } = rewriteKubeconfig(cfg, BOUGH_CA);
  assertEquals(clusters, [{ host: "plain:6443", caPem: undefined }]);
  assertStringIncludes(rewritten, btoa(BOUGH_CA));
});

Deno.test("rewrite: unparseable YAML throws (caller leaves KUBECONFIG untouched)", () => {
  assertThrows(() => rewriteKubeconfig("clusters:\n  - : : bad\n  key without", BOUGH_CA));
});

Deno.test("kubeconfigPath: first KUBECONFIG entry wins, else ~/.kube/config", () => {
  assertEquals(kubeconfigPath({ KUBECONFIG: "/a/one:/b/two", HOME: "/h" }), "/a/one");
  assertEquals(kubeconfigPath({ HOME: "/h" }), "/h/.kube/config");
});
