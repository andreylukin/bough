import { assertEquals } from "jsr:@std/assert@1";
import { augmentCloudPolicy, AWS_HOST_SUFFIX, setupKube } from "./cloud.ts";
import { decide, policy, type Request } from "./policy.ts";

const BOUGH_CA = "-----BEGIN CERTIFICATE-----\nBOUGH\n-----END CERTIFICATE-----\n";
const CLUSTER_CA = "-----BEGIN CERTIFICATE-----\nCLUSTER\n-----END CERTIFICATE-----\n";
const EKS = "eabf.gr7.us-east-2.eks.amazonaws.com";

Deno.test("augmentCloudPolicy: cluster hosts classify k8s + are trusted; aws trusted; reads flow, writes gated", () => {
  const base = policy({ mode: "review", allowHosts: new Set(["github.com"]), hostMiss: "hold" });
  const pol = augmentCloudPolicy(base, [EKS]);

  // kubectl GET → read, allowed (was hostMiss-held before)
  const get: Request = { host: EKS, method: "GET", path: "/api/v1/namespaces/dev/pods" };
  const gd = decide(get, pol);
  assertEquals(gd.verdict, "allow");
  assertEquals(gd.action.service, "k8s"); // classified as kubernetes

  // kubectl DELETE → write, held for approval in review mode
  const del: Request = { host: EKS, method: "DELETE", path: "/api/v1/namespaces/dev/pods/x" };
  assertEquals(decide(del, pol).verdict, "hold");

  // aws read flows; aws write held — host trusted via the AWS suffix
  const awsRead: Request = {
    host: "sts.us-east-2.amazonaws.com",
    method: "POST",
    path: "/",
    body: "Action=GetCallerIdentity",
  };
  assertEquals(decide(awsRead, pol).verdict, "allow");
  const awsWrite: Request = {
    host: "ec2.us-east-2.amazonaws.com",
    method: "POST",
    path: "/",
    body: "Action=TerminateInstances",
  };
  assertEquals(decide(awsWrite, pol).verdict, "hold");

  assertEquals(pol.allowHosts.has(AWS_HOST_SUFFIX), true);
  assertEquals(pol.allowHosts.has(EKS), true);
});

Deno.test("augmentCloudPolicy: an empty (allow-all) allowHosts is left empty, not flipped", () => {
  const base = policy({ mode: "review" }); // allowHosts empty = allow every host
  const pol = augmentCloudPolicy(base, [EKS]);
  assertEquals(pol.allowHosts.size, 0); // still allow-all — NOT flipped into allowlist mode
  assertEquals(pol.k8sHosts.has(EKS), true); // but k8s classification still applied
  // a non-cloud host is never rejected by the host gate (no hostMiss); its verdict
  // comes only from the action layer, exactly as before augmentation.
  const other: Request = { host: "example.com", method: "GET", path: "/" };
  assertEquals(decide(other, pol).reason.includes("allowlist"), false);
});

Deno.test("setupKube: rewrites kubeconfig to bough CA, writes it, maps host→real CA", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-cloud-" });
  const kube = await Deno.makeTempDir({ prefix: "bough-kube-" });
  try {
    const cfgPath = `${kube}/config`;
    await Deno.writeTextFile(
      cfgPath,
      [
        "clusters:",
        "- name: dev",
        "  cluster:",
        `    server: https://${EKS}`,
        `    certificate-authority-data: ${btoa(CLUSTER_CA)}`,
        "users:",
        "- name: dev",
        "  user: { exec: { command: aws } }",
      ].join("\n"),
    );
    Deno.env.set("KUBECONFIG", cfgPath);
    try {
      const setup = setupKube(BOUGH_CA, dir)!;
      assertEquals(setup.hosts, [EKS]);
      assertEquals(setup.upstreamCa.get(EKS), CLUSTER_CA); // proxy trusts the real cluster
      assertEquals(setup.clientCertUsers, []);
      // the on-disk rewrite makes kubectl trust bough
      const written = await Deno.readTextFile(setup.configPath);
      const { parse } = await import("@std/yaml");
      const doc = parse(written) as { clusters: { cluster: Record<string, string> }[] };
      assertEquals(atob(doc.clusters[0].cluster["certificate-authority-data"]), BOUGH_CA);
    } finally {
      Deno.env.delete("KUBECONFIG");
    }
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
    await Deno.remove(kube, { recursive: true }).catch(() => {});
  }
});

Deno.test("setupKube: no kubeconfig → undefined (kubectl left alone)", () => {
  Deno.env.set("KUBECONFIG", "/nonexistent/bough/kubeconfig-xyz");
  try {
    assertEquals(setupKube(BOUGH_CA), undefined);
  } finally {
    Deno.env.delete("KUBECONFIG");
  }
});
