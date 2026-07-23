/**
 * The egress firewall bough runs in-process — native Claw Patrol. It owns a MITM
 * certificate authority (ca.ts), the policy gate (gate.ts), and one intercepting proxy
 * (proxy.ts) PER SESSION, and it hands the sandbox exec path the env that routes
 * commands through their session's proxy and trusts the shared CA. No Go binary, no
 * WireGuard, no external dashboard: the audit feed + human approvals live on bough's
 * own /net/requests + Network rail.
 *
 * Per-session listeners are how egress gets attributed and policied by branch: the
 * proxy can only tell sessions apart by something on the wire, and the listening port
 * is that signal. Listeners are spun up lazily on a session's first sandboxed exec and
 * reaped on its turn.finished (commands only run inside turns; a held request keeps
 * its turn alive because the tool call is blocked on the gate). A process backgrounded
 * from a turn (`foo &`) loses its proxy when the turn ends — the next turn gets a
 * fresh one.
 *
 * On by default (opt out with BOUGH_CLAWPATROL=0). Held requests can no longer wedge a
 * turn: a human hold fails closed after a timeout (gate.ts) with a re-request message,
 * so a fail-closed default is safe. With the flag set to 0, no proxy starts and exec
 * runs unrouted.
 */
import { join } from "node:path";
import { caEnv, CertAuthority } from "./ca.ts";
import { ProxyServer } from "./proxy.ts";
import { gateHostIp } from "../sandbox/gatehost.ts";
import { sandboxVm } from "../sandbox/vmsession.ts";
import { createGate, type Gate } from "./gate.ts";
import { PluginHost, type PluginInfo, type RequestSample, specFromRequests } from "./plugins.ts";
import { caTrustCommand, isCaTrusted } from "./catrust.ts";
import { augmentCloudPolicy, type KubeSetup, setupKube } from "./cloud.ts";
import { loadConfig, type NetConfig, resolveConfig, toPolicy } from "./config.ts";
import { resolveCredentials } from "./credentials.ts";
import { brokerEnv } from "./execcred.ts";
import type { Db } from "../db/db.ts";
import type { Bus } from "../bus.ts";
import { annotateNet } from "../worker/annotate.ts";

/** Whether bough should run the egress proxy (on by default — opt out with =0). */
function clawpatrolEnabled(): boolean {
  return Deno.env.get("BOUGH_CLAWPATROL") !== "0";
}

/**
 * Per-session kubectl cache dir (KUBECACHEDIR). kubectl's discovery/HTTP cache
 * defaults to ~/.kube/cache, which the Seatbelt read-denylist covers wholesale —
 * without this, every kubectl run stalls on cache read/write denials. Temp paths
 * are already in the profile's write-allow. Created eagerly: kubectl won't mkdir -p
 * a missing parent chain itself.
 */
function kubeCacheDir(sessionId: string): string {
  const dir = join(
    Deno.env.get("TMPDIR") ?? "/tmp",
    "bough-kube-cache",
    sessionId || "default",
  );
  try {
    Deno.mkdirSync(dir, { recursive: true });
  } catch {
    // Racing a concurrent create is fine; anything else surfaces in kubectl itself.
  }
  return dir;
}

/** Useless placeholder token; the proxy overwrites the header with the real PAT. */
const GH_SENTINEL = "__bough_github_pat__";

/** GH_TOKEN/GITHUB_TOKEN sentinel iff the session has a github credential binding. */
function githubSentinelEnv(config: NetConfig): Record<string, string> {
  const hasGithub = config.credentials.some((c) =>
    c.host === "github.com" || c.host === "api.github.com" || c.host.endsWith(".github.com")
  );
  return hasGithub ? { GH_TOKEN: GH_SENTINEL, GITHUB_TOKEN: GH_SENTINEL } : {};
}

interface GatewayStatus {
  enabled: boolean;
  running: boolean;
  /** Live per-session listeners (informational; each session gets its own port). */
  listeners: number;
  caPath: string;
  /**
   * macOS: whether bough's CA is keychain-trusted. Go tools (gh, some kubectl auth
   * plugins) ignore the CA env var and need this. undefined until first checked /
   * when the proxy is off. `caTrustCommand` is the one-time fix to show when false.
   */
  caTrusted?: boolean;
  caTrustCommand?: string;
}

/** Owns the CA + gate + per-session proxies for the server's lifetime. */
export class ClawpatrolGateway {
  #db: Db;
  #bus: Bus;
  #ca?: CertAuthority;
  #gate?: Gate;
  #plugins?: PluginHost;
  #caTrusted?: boolean;
  // kubectl integration: rewritten kubeconfig + per-cluster upstream CA (cloud.ts).
  #kube?: KubeSetup;
  // Live listeners keyed by sessionId ("" = a caller with no session, e.g. tests).
  #proxies = new Map<string, ProxyServer>();
  // In-flight starts, so concurrent tool calls in one turn share one listener.
  #starting = new Map<string, Promise<ProxyServer>>();
  #running = false;
  #unsubscribe?: () => void;

  constructor(cfg: { db: Db; bus: Bus }) {
    this.#db = cfg.db;
    this.#bus = cfg.bus;
  }

  /** The gate the server puts on AppCtx so the approval endpoints share it with the proxy. */
  get gate(): Gate | undefined {
    return this.#gate;
  }

  status(): GatewayStatus {
    const caPath = this.#ca?.caCertPath ?? "";
    return {
      enabled: clawpatrolEnabled(),
      running: this.#running,
      listeners: this.#proxies.size,
      caPath,
      ...(this.#running && caPath
        ? { caTrusted: this.#caTrusted, caTrustCommand: caTrustCommand(caPath) }
        : {}),
    };
  }

  /** Re-check keychain trust (memoized; the UI hint clears once you run the command). */
  async refreshCaTrust(): Promise<boolean | undefined> {
    if (!this.#running || !this.#ca) return undefined;
    this.#caTrusted = await isCaTrusted(this.#ca.caCertPath);
    return this.#caTrusted;
  }

  /** Classifier plugins; see /net/plugins endpoints. */
  listPlugins(): { dir: string; plugins: PluginInfo[] } {
    return { dir: this.#plugins?.dir ?? "", plugins: this.#plugins?.list() ?? [] };
  }

  async reloadPlugins(): Promise<PluginInfo[]> {
    await this.#plugins?.load();
    return this.#plugins?.list() ?? [];
  }

  /** Scaffold a starter plugin file; the loader picks it up immediately. */
  async createPlugin(name: string): Promise<{ path: string; plugins: PluginInfo[] }> {
    if (!this.#plugins) throw new Error("Claw Patrol is off");
    const { path } = await this.#plugins.scaffold(name);
    return { path, plugins: this.#plugins.list() };
  }

  /** Install a drafted declarative spec into the library (validated + fixture-checked before disk). */
  async installPlugin(spec: unknown): Promise<{ path: string; plugins: PluginInfo[] }> {
    if (!this.#plugins) throw new Error("Claw Patrol is off");
    const { path } = await this.#plugins.install(spec);
    return { path, plugins: this.#plugins.list() };
  }

  /**
   * Build a plugin from selected feed requests and install it (unique name). The
   * `activate` callback owns the enable scope — session or global — and runs before
   * the return so the plugin gates immediately. Returns the new name.
   */
  async pluginFromRequests(
    samples: RequestSample[],
    activate: (name: string) => void,
  ): Promise<{ name: string; path: string; plugins: PluginInfo[] }> {
    if (!this.#plugins) throw new Error("Claw Patrol is off");
    const { path, name } = await this.#plugins.install(specFromRequests(samples), {
      uniqueName: true,
    });
    activate(name);
    return { name, path, plugins: this.#plugins.list() };
  }

  /** True when the library has a loaded plugin by this name (enable-target check). */
  hasPlugin(name: string): boolean {
    return this.#plugins?.list().some((p) => p.name === name && p.status === "loaded") ?? false;
  }

  /** Boot the CA, gate, and plugins when enabled; listeners start lazily per session. */
  async start(): Promise<void> {
    if (!clawpatrolEnabled()) return;
    this.#ca = CertAuthority.load();
    this.#plugins = new PluginHost();
    await this.#plugins.load();
    const plugins = this.#plugins;
    // kubectl: rewrite the operator's kubeconfig so the sandbox trusts bough's CA,
    // and learn each cluster's real CA for upstream trust (cloud.ts). aws needs no
    // rewrite — AWS_CA_BUNDLE (ca.caEnv) + public roots cover it.
    this.#kube = setupKube(this.#ca.caCertPem);
    const kubeHosts = this.#kube?.hosts ?? [];
    if (this.#kube) {
      console.log(`[clawpatrol] kubectl: ${kubeHosts.length} cluster host(s) trusted + gated`);
      if (this.#kube.clientCertUsers.length) {
        console.warn(
          `[clawpatrol] kubeconfig users on client-cert auth won't work through the ` +
            `proxy (mTLS can't survive MITM): ${this.#kube.clientCertUsers.join(", ")}`,
        );
      }
    }
    // Trust + classify the cloud CLI hosts (k8s clusters + *.amazonaws.com) on every
    // compiled policy, so reads flow and only writes gate — see cloud.augmentCloudPolicy.
    const compile = (sessionId?: string) =>
      augmentCloudPolicy(toPolicy(resolveConfig(this.#db, sessionId).config), kubeHosts);
    this.#gate = createGate({
      db: this.#db,
      bus: this.#bus,
      policy: augmentCloudPolicy(toPolicy(loadConfig()), kubeHosts),
      // Branch policy: a session's own net_policies row, else the nearest
      // ancestor's, else the global rule set (config.ts resolveConfig).
      resolve: compile,
      // The runtime join: the branch's effective activations (inherited like every
      // other rule) select from the plugin library. Resolved per request, so
      // enable/disable edits and per-activation TTLs apply on the next gate().
      classifiers: (sessionId) =>
        plugins.activeFor(resolveConfig(this.#db, sessionId).config.plugins),
      guards: (sessionId) =>
        plugins.activeGuardsFor(resolveConfig(this.#db, sessionId).config.plugins),
      // Held requests get a local-worker one-liner on the approval card. Wired
      // here (production only) so gate tests stay hermetic.
      annotator: annotateNet,
    });
    // When a session's turn ends: expire any holds it left parked (an interrupted
    // turn's command dies but its gate hold would otherwise pend forever), then
    // reap its listener; the next turn re-acquires one.
    this.#unsubscribe = this.#bus.subscribe((e) => {
      if (e.type === "turn.finished" && e.sessionId) {
        this.#gate?.expireHolds(e.sessionId, "expired — turn ended before approval");
        void this.release(e.sessionId);
      }
    });
    this.#running = true;
    void this.refreshCaTrust(); // warm the keychain-trust hint (non-blocking)
    console.log(`[clawpatrol] native egress gateway up (per-session listeners)`);
    console.log(`[clawpatrol] sandbox clients trust the MITM CA at ${this.#ca.caCertPath}`);
  }

  async stop(): Promise<void> {
    this.#running = false;
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
    const live = [...this.#proxies.values()];
    this.#proxies.clear();
    await Promise.all(live.map((p) => p.stop()));
  }

  /** The session's live proxy, starting one if needed. */
  #acquire(key: string): Promise<ProxyServer> {
    const live = this.#proxies.get(key);
    if (live) return Promise.resolve(live);
    let starting = this.#starting.get(key);
    if (!starting) {
      const gate = this.#gate!;
      const proxy = new ProxyServer({
        ca: this.#ca!,
        gate: (req, opts) => gate.gate(req, opts),
        sessionId: key || undefined,
        // VM backend: bind the gate host so the guest can reach it (loopback is the
        // guest's own, not the host's). `proxy.url` then advertises that address.
        host: sandboxVm() ? gateHostIp() : undefined,
        // Trust each k8s cluster's private CA when re-originating (EKS serving certs
        // aren't public-rooted). Empty/absent for everyone else = default trust.
        upstreamCa: this.#kube?.upstreamCa,
        // All host-side credential injection for this session: bundle bindings from the
        // resolved config (env-var tokens, read per request) plus the kube exec creds
        // (aws eks get-token, ...). The sandbox's kubeconfig/tools carry no auth — the
        // proxy is the sole credential holder (credentials.ts).
        credentials: resolveCredentials(
          resolveConfig(this.#db, key || undefined).config,
          this.#kube?.credentials,
        ),
      });
      starting = proxy.start().then(() => {
        this.#starting.delete(key);
        this.#proxies.set(key, proxy);
        return proxy;
      });
      this.#starting.set(key, starting);
    }
    return starting;
  }

  /** Stop and forget a session's listener (turn ended / session archived). */
  async release(sessionId: string): Promise<void> {
    await this.#starting.get(sessionId)?.catch(() => {});
    const proxy = this.#proxies.get(sessionId);
    if (!proxy) return;
    this.#proxies.delete(sessionId);
    await proxy.stop();
  }

  /**
   * Env for a sandboxed command: point its HTTP(S) client at ITS SESSION's proxy and
   * trust the MITM CA. NO_PROXY keeps loopback (bough's own server, and the proxy
   * itself) direct so requests don't loop. Empty when the gateway isn't running.
   */
  async envFor(sessionId?: string): Promise<Record<string, string>> {
    if (!this.#running || !this.#ca || !this.#gate) return {};
    const proxy = await this.#acquire(sessionId ?? "");
    const url = proxy.url;
    return {
      HTTP_PROXY: url,
      HTTPS_PROXY: url,
      http_proxy: url,
      https_proxy: url,
      NO_PROXY: "localhost,127.0.0.1",
      no_proxy: "localhost,127.0.0.1",
      // The owning branch, so in-session tooling (e.g. the /net-plugin skill) can
      // scope its API calls (?session=$BOUGH_SESSION) without guessing.
      ...(sessionId ? { BOUGH_SESSION: sessionId } : {}),
      // The bough API port, always set so instructions can say $BOUGH_PORT — the
      // ${BOUGH_PORT:-4321} shell-default form breaks inside the JS template
      // literals run_steps programs are written in.
      BOUGH_PORT: Deno.env.get("BOUGH_PORT") ?? "4321",
      // GitHub sentinel: when a github credential binding is installed, gh needs *a*
      // token to send an authenticated request at all — the proxy overwrites the
      // Authorization header with the real PAT for github hosts. The sentinel itself
      // is useless (fails closed at github if the MITM is ever bypassed).
      ...githubSentinelEnv(resolveConfig(this.#db, sessionId).config),
      // Host-path / loopback env is meaningful only for a host-side (Seatbelt) child.
      // In the VM guest the CA is baked into the trust store (so no caEnv), and the
      // kube/broker host paths and loopback aren't reachable — kubectl/AWS get their
      // own guest-side delivery (kubeconfig/CA into the guest, broker on the gate IP).
      ...(sandboxVm() ? {} : {
        // kubectl reads clusters from KUBECONFIG; point it at the CA-rewritten copy so
        // it trusts the proxy's leaf. Absent when there's no kubeconfig to rewrite.
        ...(this.#kube
          ? { KUBECONFIG: this.#kube.configPath, KUBECACHEDIR: kubeCacheDir(sessionId ?? "") }
          : {}),
        // AWS read-only creds via the local broker (container-credentials protocol).
        ...brokerEnv(),
        ...caEnv(this.#ca.caCertPath),
      }),
    };
  }
}

// The process-wide gateway, set by the server on boot so the exec path (bash.ts) can
// reach it without threading it through every tool signature. Undefined in tests.
let active: ClawpatrolGateway | undefined;
export function setActiveGateway(g: ClawpatrolGateway | undefined): void {
  active = g;
}
export function activeGateway(): ClawpatrolGateway | undefined {
  return active;
}

/** The sandbox-exec env from the active gateway; empty when the proxy isn't running. */
export function clawpatrolEnv(sessionId?: string): Promise<Record<string, string>> {
  return active?.envFor(sessionId) ?? Promise.resolve({});
}
