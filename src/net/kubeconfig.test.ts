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

  // exec auth is preserved untouched; no client-cert users here
  assertStringIncludes(rewritten, "command: aws");
  assertStringIncludes(rewritten, "get-token");
  assertEquals(clientCertUsers, []);
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
