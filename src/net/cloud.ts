/**
 * Cloud CLI integration — make `kubectl` and `aws` work through Claw Patrol out of
 * the box, without the operator hand-editing the rule set.
 *
 * Both are operator CLIs whose API endpoints should be TRUSTED at the host level (so
 * reads flow) but GATED at the action level (writes held/denied). The host gate runs
 * before classification, so without this every kubectl/aws call would hold on
 * `hostMiss` before its verb was ever classified. `augmentCloudPolicy` adds the cloud
 * hosts to the trusted set and marks the k8s ones for kubernetes classification —
 * reads pass, writes stay gated by mode/verbs (SigV4 stays valid: we never modify
 * AWS requests, only read them).
 *
 * kubectl also needs the CA rewrite (`setupKube`): it validates the API-server cert
 * against the CA in its kubeconfig, so the sandbox gets a rewritten kubeconfig whose
 * cluster CA is bough's, and the proxy learns each cluster's REAL CA to trust
 * upstream (EKS serving certs are signed by the private cluster CA — see proxy.ts
 * upstreamCa). `aws` needs no such rewrite: AWS endpoints use public roots (proxy
 * default trust) and the AWS CLI already honors AWS_CA_BUNDLE (set in ca.caEnv).
 */
import { dirname, join } from "node:path";
import { mkdirSync, writeFileSync } from "node:fs";
import { kubeconfigPath, loadKubeconfig, rewriteKubeconfig } from "./kubeconfig.ts";
import { netDir } from "./install.ts";
import type { Policy } from "./policy.ts";

/** AWS API hosts trusted (host-level) so `aws` works; writes still action-gated. */
export const AWS_HOST_SUFFIX = "*.amazonaws.com";

export interface KubeSetup {
  /** The rewritten kubeconfig the sandbox points KUBECONFIG at (cluster CA = bough's). */
  configPath: string;
  /** Cluster API-server hosts — trusted + classified as kubernetes. */
  hosts: string[];
  /** host → the cluster's REAL CA PEM, for the proxy to trust upstream. */
  upstreamCa: Map<string, string>;
  /** Users on client-cert auth (breaks under MITM — caller warns). */
  clientCertUsers: string[];
}

/**
 * Read the operator's kubeconfig and produce the sandbox-facing rewrite: a kubeconfig
 * whose cluster CAs are bough's (written to <netDir>/kubeconfig), plus the per-host
 * real CA map and cluster host list. Returns undefined when there's no kubeconfig or
 * it can't be parsed — kubectl then simply isn't set up (no KUBECONFIG injected).
 */
export function setupKube(boughCaPem: string, dir = netDir()): KubeSetup | undefined {
  const src = kubeconfigPath();
  const text = loadKubeconfig(src);
  if (!text) return undefined;
  let rewritten;
  try {
    rewritten = rewriteKubeconfig(text, boughCaPem, src ? dirname(src) : "");
  } catch {
    return undefined; // unparseable → leave kubectl alone rather than break it
  }
  if (rewritten.clusters.length === 0) return undefined;

  const configPath = join(dir, "kubeconfig");
  mkdirSync(dir, { recursive: true });
  writeFileSync(configPath, rewritten.rewritten, { mode: 0o600 });

  const upstreamCa = new Map<string, string>();
  for (const c of rewritten.clusters) if (c.caPem) upstreamCa.set(c.host, c.caPem);

  return {
    configPath,
    hosts: rewritten.clusters.map((c) => c.host),
    upstreamCa,
    clientCertUsers: rewritten.clientCertUsers,
  };
}

/**
 * Trust + classify the cloud CLI hosts on a compiled policy. k8s cluster hosts get
 * kubernetes classification always; cloud hosts join the allowlist ONLY when one is
 * already in force (a non-empty allowHosts) — an empty allowHosts means "allow every
 * host", so adding entries there would wrongly flip it into allowlist mode.
 */
export function augmentCloudPolicy(pol: Policy, kubeHosts: readonly string[]): Policy {
  const allowHosts = pol.allowHosts.size > 0
    ? new Set([...pol.allowHosts, AWS_HOST_SUFFIX, ...kubeHosts])
    : pol.allowHosts;
  return {
    ...pol,
    allowHosts,
    k8sHosts: new Set([...pol.k8sHosts, ...kubeHosts]),
  };
}
