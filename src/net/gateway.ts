/**
 * The Claw Patrol gateway bough runs and supervises. This is the "full dependency"
 * seam: instead of reimplementing the firewall in TypeScript, bough composes a gateway
 * config from its installed policy bundles, boots the real `clawpatrol gateway`, and
 * routes every sandboxed shell command through `clawpatrol run` so egress is captured,
 * gated, and credential-injected by Claw Patrol at L3.
 *
 * Opt-in: enabled only when BOUGH_CLAWPATROL=1, because routing traffic through the
 * gateway requires a one-time device onboarding that bough can't do headlessly —
 * `clawpatrol join <dashboard-url>` (registers a WireGuard peer, gets approved on the
 * dashboard, assigns a profile, installs the CA). Until that join is done, Claw Patrol
 * captures traffic but has nowhere to route it; enabling the flag before joining will
 * break egress rather than gate it. With the flag off, bough runs exactly as before.
 *
 * The gateway owns the audit feed and human approvals on its own dashboard (there is no
 * programmatic approver/events API), so bough links operators to it rather than
 * mirroring it — see GET /net/status and the web Network rail.
 */
import { join } from "node:path";
import { homedir } from "node:os";
import { defaultGateway, type HclEndpoint, type HclPolicy, type HclRule, renderHcl } from "./hcl.ts";
import { getBundle, listBundles } from "./bundles.ts";
import { isInstalled, netDir } from "./install.ts";
import { clawpatrolAvailable } from "./clawpatrol.ts";

const DASHBOARD = () => Deno.env.get("BOUGH_CLAWPATROL_DASHBOARD") ?? "127.0.0.1:8090";

/** Whether bough should run and route through Claw Patrol (opt-in — see module docs). */
export function clawpatrolEnabled(): boolean {
  return Deno.env.get("BOUGH_CLAWPATROL") === "1";
}

/** The clawpatrol binary (BOUGH_CLAWPATROL_BIN overrides). */
function bin(): string {
  return Deno.env.get("BOUGH_CLAWPATROL_BIN") ?? "clawpatrol";
}

/**
 * True when this machine already has a Claw Patrol client joined to a reachable
 * gateway (Clawpatrol.app or a prior `clawpatrol join`). In that case bough must NOT
 * spawn its own gateway — it would fight the existing one for the WireGuard port — it
 * routes through the existing gateway via `clawpatrol run`.
 */
export function existingGatewayReachable(): boolean {
  try {
    const out = new Deno.Command(bin(), { args: ["status"], stdout: "piped", stderr: "null" })
      .outputSync();
    return new TextDecoder().decode(out.stdout).includes("gateway reachable");
  } catch {
    return false;
  }
}

/**
 * CA-trust env for sandboxed commands, so TLS clients trust the gateway's MITM CA
 * (the gateway terminates TLS to gate + inject). Empty when the CA isn't present.
 * Mirrors `clawpatrol env`'s exports; keyed to ~/.clawpatrol/ca.crt.
 */
export function clawpatrolCaEnv(): Record<string, string> {
  const ca = join(homedir(), ".clawpatrol", "ca.crt");
  try {
    Deno.statSync(ca);
  } catch {
    return {};
  }
  return {
    SSL_CERT_FILE: ca,
    NODE_EXTRA_CA_CERTS: ca,
    REQUESTS_CA_BUNDLE: ca,
    CURL_CA_BUNDLE: ca,
    GIT_SSL_CAINFO: ca,
    DENO_CERT: ca,
    PIP_CERT: ca,
    AWS_CA_BUNDLE: ca,
  };
}

/**
 * Compose one gateway HCL from every installed bundle's fragment plus a single gateway
 * block. Bundles render from their defaults here; per-install params are a follow-up
 * (the shipped `github` bundle's defaults are read-allowed / write-gated).
 */
export function composeGatewayHcl(stateDir: string): string {
  const endpoints: HclEndpoint[] = [];
  const rules: HclRule[] = [];
  const credentials: HclPolicy["credentials"] = [];
  for (const manifest of listBundles()) {
    if (!isInstalled(manifest.name)) continue;
    const bundle = getBundle(manifest.name);
    if (!bundle) continue;
    const defaults: Record<string, unknown> = {};
    for (const p of bundle.params) if (p.default !== undefined) defaults[p.name] = p.default;
    const frag = bundle.render(defaults);
    endpoints.push(...frag.endpoints);
    rules.push(...frag.rules);
    credentials.push(...frag.credentials);
  }
  const dash = DASHBOARD();
  const policy: HclPolicy = {
    gateway: defaultGateway({
      dashboardListen: dash,
      publicUrl: `http://${dash}`,
      stateDir,
    }),
    credentials,
    endpoints,
    rules,
    profiles: [],
  };
  return renderHcl(policy);
}

export interface GatewayStatus {
  enabled: boolean;
  available: boolean; // clawpatrol binary present
  running: boolean; // routing is active (own gateway healthy, or an existing one reachable)
  external: boolean; // using a pre-existing gateway (Clawpatrol.app) rather than bough's own
  dashboardUrl: string;
}

/** Supervises the `clawpatrol gateway` child process for the server's lifetime. */
export class ClawpatrolGateway {
  #child?: Deno.ChildProcess;
  #running = false;
  #external = false;
  #dir: string;

  constructor(dir = join(netDir(), "gateway")) {
    this.#dir = dir;
  }

  status(): GatewayStatus {
    return {
      enabled: clawpatrolEnabled(),
      available: clawpatrolAvailable(),
      running: this.#running,
      external: this.#external,
      dashboardUrl: this.#external ? "" : `http://${DASHBOARD()}`,
    };
  }

  /**
   * Route through Claw Patrol: if the machine already has a joined gateway
   * (Clawpatrol.app), use it — do NOT spawn our own (they'd fight for the WG port).
   * Otherwise render a config from bough's bundles and boot a gateway here.
   */
  async start(): Promise<void> {
    if (!clawpatrolEnabled()) return;
    if (!clawpatrolAvailable()) {
      console.warn("[clawpatrol] BOUGH_CLAWPATROL=1 but the clawpatrol binary isn't on PATH — egress is NOT gated");
      return;
    }
    if (existingGatewayReachable()) {
      this.#external = true;
      this.#running = true;
      console.log("[clawpatrol] using the existing joined gateway (Clawpatrol.app) — routing sandbox egress through it");
      return;
    }
    await Deno.mkdir(this.#dir, { recursive: true });
    const cfgPath = join(this.#dir, "gateway.hcl");
    await Deno.writeTextFile(cfgPath, composeGatewayHcl(join(this.#dir, "state")));

    const args = ["gateway", cfgPath];
    const pw = Deno.env.get("BOUGH_CLAWPATROL_DASHBOARD_PW");
    if (pw) args.splice(1, 0, "--set-dashboard-password", pw);
    // Inherit stderr so gateway logs surface in bough's console; piping without
    // draining would buffer-block the child once the pipe fills.
    this.#child = new Deno.Command(bin(), { args, stdout: "null", stderr: "inherit" }).spawn();

    if (await this.#waitHealthy()) {
      this.#running = true;
      console.log(`[clawpatrol] gateway up — dashboard ${this.status().dashboardUrl}`);
      console.log("[clawpatrol] first run: `clawpatrol join " + this.status().dashboardUrl +
        "` on this machine to route traffic (registers the device + installs the CA)");
    } else {
      console.warn("[clawpatrol] gateway did not become healthy — egress is NOT gated");
    }
  }

  async stop(): Promise<void> {
    this.#running = false;
    if (!this.#child) return;
    try {
      this.#child.kill("SIGTERM");
      await this.#child.status;
    } catch { /* already gone */ }
    this.#child = undefined;
  }

  /** Poll the dashboard until it answers (up to ~8s). */
  async #waitHealthy(): Promise<boolean> {
    const url = this.status().dashboardUrl;
    for (let i = 0; i < 16; i++) {
      await new Promise((r) => setTimeout(r, 500));
      try {
        const res = await fetch(url, { signal: AbortSignal.timeout(1000) });
        await res.body?.cancel();
        return true;
      } catch { /* not up yet */ }
    }
    return false;
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

/**
 * The argv prefix that routes a command through the gateway, or [] when not routing.
 * `clawpatrol run -- <cmd>` puts the command's process tree behind Claw Patrol's L3
 * capture. Only prefixes when the gateway is healthy — otherwise commands run unrouted
 * (fail-open) so a misconfigured install doesn't wedge every turn.
 */
export function clawpatrolRunPrefix(gateway = active): string[] {
  if (!gateway || !gateway.status().running) return [];
  return [bin(), "run", "--"];
}
