/**
 * kubeconfig rewriting for the kubectl MITM path. kubectl validates the API-server
 * cert against the CA baked into its kubeconfig (`certificate-authority-data`), NOT
 * the system store or SSL_CERT_FILE — so for Claw Patrol to terminate that TLS, the
 * sandbox must see a kubeconfig whose cluster CA is bough's MITM CA. This module
 * reads the operator's kubeconfig, swaps every cluster CA for bough's, and reports:
 *   - the rewritten YAML (written to the session dir, pointed at by KUBECONFIG), and
 *   - each cluster's ORIGINAL CA keyed by host, so the proxy can trust the real
 *     cluster upstream when it re-originates (EKS serving certs are signed by the
 *     private cluster CA, not a public root — see proxy.ts upstreamCa).
 *
 * Auth is left untouched: exec plugins (aws eks get-token) and bearer tokens survive
 * MITM. Client-certificate auth does NOT — the client cert is negotiated to the proxy,
 * not the origin — so users carrying `client-certificate(-data)` are reported for a
 * warning; their clusters won't authenticate through the proxy.
 */
import { parse, stringify } from "@std/yaml";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

export interface ClusterCa {
  /** Lowercased host[:port] of the cluster's API server (matches the gate). */
  host: string;
  /** The cluster's real CA in PEM, for the proxy to trust upstream. Absent if none. */
  caPem?: string;
}

/**
 * An exec credential plugin lifted out of a kubeconfig user (aws eks get-token,
 * gke-gcloud-auth-plugin, ...). The HOST runs it — the sandbox can't (the plugin
 * reads ~/.aws etc., which the seatbelt denies) — and the proxy stamps the minted
 * bearer token onto requests to `host` (see cloud.ts / proxy.ts CredentialRule).
 */
export interface ExecCredSpec {
  /** Cluster API-server host[:port] this credential authenticates. */
  host: string;
  command: string;
  args: string[];
  /** exec.env entries ({name, value} list in the kubeconfig) flattened to a map. */
  env: Record<string, string>;
}

export interface RewriteResult {
  /** The rewritten kubeconfig YAML: every cluster CA replaced with bough's. */
  rewritten: string;
  /** Original CA per cluster host — feeds the proxy's upstream trust + k8sHosts. */
  clusters: ClusterCa[];
  /** Names of users relying on client-cert auth (breaks under MITM; caller warns). */
  clientCertUsers: string[];
  /** Exec plugins stripped from users, mapped to their cluster hosts via contexts. */
  execCreds: ExecCredSpec[];
}

// deno-lint-ignore no-explicit-any
type Doc = any;

/** The active kubeconfig path: first KUBECONFIG entry, else ~/.kube/config. undefined if none. */
export function kubeconfigPath(
  env: Record<string, string | undefined> = Deno.env.toObject(),
): string | undefined {
  const fromEnv = env.KUBECONFIG?.split(":").map((p) => p.trim()).find(Boolean);
  return fromEnv ?? join(env.HOME ?? homedir(), ".kube", "config");
}

/** Read the active kubeconfig text, or null if there isn't one. */
export function loadKubeconfig(path = kubeconfigPath()): string | null {
  if (!path) return null;
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

function hostOf(server: unknown): string | undefined {
  if (typeof server !== "string") return undefined;
  try {
    return new URL(server).host.toLowerCase();
  } catch {
    return undefined;
  }
}

/** The cluster's real CA PEM from inline data (base64) or a file path; undefined if neither. */
function clusterCaPem(cluster: Doc, baseDir: string): string | undefined {
  const data = cluster["certificate-authority-data"];
  if (typeof data === "string" && data) {
    try {
      return atob(data);
    } catch {
      return undefined;
    }
  }
  const file = cluster["certificate-authority"];
  if (typeof file === "string" && file) {
    try {
      return readFileSync(file.startsWith("/") ? file : join(baseDir, file), "utf8");
    } catch {
      return undefined;
    }
  }
  return undefined;
}

/**
 * Rewrite a kubeconfig so every cluster trusts `boughCaPem`, returning the new YAML
 * plus each cluster's original CA (by host) for upstream trust. `baseDir` resolves
 * relative `certificate-authority` file paths (the kubeconfig's own directory).
 * Throws on unparseable YAML — the caller falls back to leaving KUBECONFIG untouched.
 */
export function rewriteKubeconfig(text: string, boughCaPem: string, baseDir = ""): RewriteResult {
  const doc = parse(text) as Doc;
  const clusters: ClusterCa[] = [];
  const clientCertUsers: string[] = [];
  const boughData = btoa(boughCaPem);
  const clusterHostByName = new Map<string, string>();

  for (const entry of doc?.clusters ?? []) {
    const cluster = entry?.cluster;
    if (!cluster) continue;
    const host = hostOf(cluster.server);
    const caPem = clusterCaPem(cluster, baseDir);
    if (host) {
      clusters.push({ host, caPem });
      if (typeof entry.name === "string") clusterHostByName.set(entry.name, host);
    }
    // Point kubectl at bough's CA; drop any file ref so only the inline data is used.
    cluster["certificate-authority-data"] = boughData;
    delete cluster["certificate-authority"];
    // TLS SNI/host verification: the proxy presents a leaf for the real host, so the
    // kubeconfig server URL stays as-is — no insecure-skip-tls-verify needed.
  }

  // Lift exec plugins out of users: the sandbox can't run them (they read ~/.aws
  // and friends, seatbelt-denied), so the HOST mints the token and the proxy stamps
  // it (cloud.ts). Stripping the block keeps in-sandbox kubectl from trying anyway
  // and failing; the request goes out unauthenticated and the proxy adds the header.
  const execByUser = new Map<string, { command: string; args: string[]; env: Record<string, string> }>();
  for (const entry of doc?.users ?? []) {
    const user = entry?.user;
    if (!user) continue;
    if (user["client-certificate"] || user["client-certificate-data"]) {
      clientCertUsers.push(entry.name ?? "(unnamed)");
    }
    const exec = user.exec;
    if (exec && typeof exec.command === "string" && typeof entry.name === "string") {
      const env: Record<string, string> = {};
      for (const e of Array.isArray(exec.env) ? exec.env : []) {
        if (typeof e?.name === "string" && typeof e?.value === "string") env[e.name] = e.value;
      }
      execByUser.set(entry.name, {
        command: exec.command,
        args: Array.isArray(exec.args) ? exec.args.filter((a: unknown) => typeof a === "string") : [],
        env,
      });
      delete user.exec;
    }
  }

  // Pair users with clusters via contexts (first pairing per host wins — matches
  // kubectl's own resolution, where a context names one user per cluster).
  const execCreds: ExecCredSpec[] = [];
  const taken = new Set<string>();
  for (const entry of doc?.contexts ?? []) {
    const ctx = entry?.context;
    const host = clusterHostByName.get(ctx?.cluster);
    const exec = execByUser.get(ctx?.user);
    if (!host || !exec || taken.has(host)) continue;
    taken.add(host);
    execCreds.push({ host, ...exec });
  }

  return { rewritten: stringify(doc), clusters, clientCertUsers, execCreds };
}
