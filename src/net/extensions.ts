/**
 * Programmable policy extensions — mitmproxy-addon-style guards, but native: user
 * TypeScript modules in ~/.bough/net/extensions/*.ts that run in the gate path with
 * the full decrypted request. The static NetConfig covers "which hosts/verbs"; an
 * extension covers everything it can't — cross-request invariants ("only merge a PR
 * on a branch this session created"), out-of-band verification (call the gh API and
 * look at the PR before deciding), body inspection beyond the built-in classifiers.
 *
 * Contract (a module may export any subset):
 *   export const name = "gh-merge-guard";          // default: filename
 *   export async function gate(req, ctx) { ... }   // the guard
 *
 * gate() returns a Verdict ("allow" | "deny" | "hold"), {verdict, reason}, or
 * undefined to pass. The FIRST extension returning a verdict overrides the static
 * decision (which rides along in ctx.decision); undefined from everyone means the
 * static rule set stands. A throw or a timeout (10s) logs, records the error, and
 * falls through — the static posture still gates, so a broken extension can't open
 * the firewall.
 *
 * ctx gives extensions state + reach:
 *   ctx.sessionId  — the branch that owns this egress (undefined for unattributed)
 *   ctx.action     — the classified action ("graphql:mutation", "PUT /repos/…")
 *   ctx.decision   — what the static rule set decided (extension may veto/override)
 *   ctx.state      — per-extension persistent KV (DB-backed, survives restarts)
 *   ctx.fetch      — plain fetch; runs in the SERVER process, so it egresses
 *                    directly (no proxy env → no gate recursion) and may use env
 *                    credentials the sandbox never sees
 *   ctx.bodyText   — the decrypted request body as text
 *
 * TRUST MODEL: extensions are the operator's own code and run with the server's
 * full permissions — the same standing as mitmproxy addons. Do not load files you
 * would not run as yourself.
 */
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { existsSync, readdirSync } from "node:fs";
import { bodyText, type Decision, type Request, type Verdict } from "./policy.ts";
import { netDir } from "./install.ts";
import type { Db } from "../db/db.ts";

export interface GuardCtx {
  sessionId?: string;
  action: Decision["action"];
  decision: Decision;
  state: {
    get: (key: string) => unknown;
    set: (key: string, value: unknown) => void;
    delete: (key: string) => void;
  };
  fetch: typeof fetch;
  bodyText: string;
}

export type GuardResult = Verdict | { verdict: Verdict; reason?: string } | undefined;

export interface Guard {
  name: string;
  file?: string;
  gate: (req: Request, ctx: GuardCtx) => GuardResult | Promise<GuardResult>;
}

export interface ExtensionInfo {
  name: string;
  file: string;
  /** Load or compile error; a broken file is listed but never gates. */
  error?: string;
}

const GATE_TIMEOUT_MS = 10_000;

export interface ExtensionHostOpts {
  /** Guard timeout override (tests). */
  timeoutMs?: number;
  /** fetch given to guards via ctx.fetch (tests stub the gh API with this). */
  fetchImpl?: typeof fetch;
}

/** The default extensions dir: <netDir>/extensions (created on demand by the user). */
export function extensionsDir(dir = netDir()): string {
  return join(dir, "extensions");
}

export class ExtensionHost {
  #db: Db;
  #guards: Guard[] = [];
  #errors: ExtensionInfo[] = [];
  #timeoutMs: number;
  #fetch: typeof fetch;

  constructor(db: Db, opts: ExtensionHostOpts = {}) {
    this.#db = db;
    this.#timeoutMs = opts.timeoutMs ?? GATE_TIMEOUT_MS;
    this.#fetch = opts.fetchImpl ?? globalThis.fetch;
  }

  /** Loaded + broken extensions, for GET /net/extensions and the logs. */
  list(): ExtensionInfo[] {
    return [
      ...this.#guards.map((g) => ({ name: g.name, file: g.file ?? "" })),
      ...this.#errors,
    ];
  }

  /** Register an in-process guard (tests, built-ins). */
  register(guard: Guard): void {
    this.#guards.push(guard);
  }

  /**
   * (Re)load every *.ts/*.js module in `dir`. A cache-busting query makes reload
   * pick up edits (Deno caches module URLs). Broken files are recorded, not fatal.
   */
  async load(dir = extensionsDir()): Promise<void> {
    this.#guards = [];
    this.#errors = [];
    if (!existsSync(dir)) return;
    const files = readdirSync(dir).filter((f) => /\.(ts|js|mts|mjs)$/.test(f)).sort();
    for (const f of files) {
      const path = join(dir, f);
      try {
        const mod = await import(`${pathToFileURL(path).href}?v=${Date.now()}`);
        if (typeof mod.gate !== "function") {
          throw new Error("module does not export a gate(req, ctx) function");
        }
        this.#guards.push({
          name: typeof mod.name === "string" ? mod.name : f.replace(/\.[^.]+$/, ""),
          file: path,
          gate: mod.gate,
        });
      } catch (e) {
        this.#errors.push({ name: f, file: path, error: (e as Error).message });
        console.error(`[clawpatrol] extension ${f} failed to load: ${(e as Error).message}`);
      }
    }
    if (this.#guards.length) {
      console.log(
        `[clawpatrol] ${this.#guards.length} extension(s): ${
          this.#guards.map((g) => g.name).join(", ")
        }`,
      );
    }
  }

  /**
   * Run the guard chain; the first verdict wins. undefined = the static decision
   * stands. Errors and timeouts fall through to the next guard.
   */
  async gate(
    req: Request,
    decision: Decision,
    sessionId?: string,
  ): Promise<{ verdict: Verdict; reason: string; by: string } | undefined> {
    for (const g of this.#guards) {
      const ctx: GuardCtx = {
        sessionId,
        action: decision.action,
        decision,
        state: this.#stateFor(g.name),
        fetch: this.#fetch,
        bodyText: bodyText(req),
      };
      try {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const out = await Promise.race([
          Promise.resolve(g.gate(req, ctx)),
          new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(new Error("guard timed out")), this.#timeoutMs);
          }),
        ]).finally(() => clearTimeout(timer));
        if (out === undefined) continue;
        const verdict = typeof out === "string" ? out : out.verdict;
        const reason = (typeof out === "object" && out.reason) ||
          `extension ${g.name}: ${verdict}`;
        return { verdict, reason, by: g.name };
      } catch (e) {
        console.error(`[clawpatrol] extension ${g.name} errored: ${(e as Error).message}`);
      }
    }
    return undefined;
  }

  #stateFor(ext: string): GuardCtx["state"] {
    return {
      get: (key) => {
        const raw = this.#db.getExtState(ext, key);
        return raw === undefined ? undefined : JSON.parse(raw);
      },
      set: (key, value) => this.#db.setExtState(ext, key, JSON.stringify(value ?? null)),
      delete: (key) => this.#db.deleteExtState(ext, key),
    };
  }
}
