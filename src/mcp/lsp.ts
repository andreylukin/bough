/**
 * First-class LSP verbs, backed by the `leta` CLI: symbol navigation is a core
 * capability like read/bash, so it is bridged into every supervisor program as
 * `lsp.*` host functions — but ONLY the curated verbs below, with bough-owned
 * names, so the model-facing surface stays stable if the backing tool changes
 * (it already survived one swap: serena-over-MCP → leta).
 *
 * leta (github.com/andreasjansson/leta) is a plain subprocess, not an MCP
 * server: a per-user daemon keeps language servers warm across sessions and
 * answers in milliseconds, which removes the MCP initialize-timeout failure
 * class the serena backend had. The confinement trade-off is deliberate: leta
 * and the language servers its daemon spawns run host-side, outside seatbelt
 * and the egress gate — acceptable because every verb except `rename` only
 * reads the workspace, and leta talks to its local daemon, never the network.
 *
 * Lazy by design: nothing is spawned at turn start. The first lsp.* call
 * registers the session workspace with the daemon (`leta workspace add`,
 * idempotent); later calls (and later turns) reuse the warm language server.
 */
import type { SpawnCtx } from "./manager.ts";

/** Where the binary may live beyond $PATH — the launchd-spawned server's PATH
 * lacks the Homebrew bins an interactive shell has. */
const EXTRA_BIN_DIRS = ["/opt/homebrew/bin", "/usr/local/bin"];

/** Absolute path to the leta binary, or undefined when not installed. */
export function letaBin(): string | undefined {
  const dirs = [...(Deno.env.get("PATH")?.split(":") ?? []), ...EXTRA_BIN_DIRS];
  for (const dir of dirs) {
    if (!dir) continue;
    try {
      const path = `${dir}/leta`;
      if (Deno.statSync(path).isFile) return path;
    } catch {
      // not here — keep looking
    }
  }
  return undefined;
}

/** True when leta is installed — the turn runner's bridge/prompt gate. */
export function lspAvailable(): boolean {
  return letaBin() !== undefined;
}

type Args = Record<string, unknown>;

function str(a: Args, key: string): string {
  const v = a[key];
  if (typeof v !== "string" || v === "") {
    throw new Error(`lsp: "${key}" (non-empty string) is required`);
  }
  return v;
}

/** Optional --context N flag (verbs that support surrounding lines). */
function context(a: Args): string[] {
  return typeof a.context === "number" ? ["--context", String(a.context)] : [];
}

/** verb → leta argv. Curated: navigation + rename, nothing else — leta's
 * daemon/workspace management stays ours (see createLspBridge). */
const VERBS: Record<string, (a: Args) => string[]> = {
  find: (a) => [
    "grep",
    str(a, "pattern"),
    ...(typeof a.path === "string" ? [a.path] : []),
  ],
  overview: (a) => ["grep", ".", str(a, "path")],
  show: (a) => ["show", str(a, "symbol"), ...context(a)],
  def: (a) => ["declaration", str(a, "symbol")],
  refs: (a) => ["refs", str(a, "symbol"), ...context(a)],
  impls: (a) => ["implementations", str(a, "symbol")],
  calls: (a) => {
    const to = typeof a.to === "string" ? a.to : undefined;
    const from = typeof a.from === "string" ? a.from : undefined;
    if (!to === !from) {
      throw new Error('lsp.calls: exactly one of "to" or "from" is required');
    }
    return ["calls", ...(to ? ["--to", to] : ["--from", from!])];
  },
  rename: (a) => ["rename", str(a, "symbol"), str(a, "new_name")],
};

/** The system-prompt section for turns that have lsp.* bridged. */
export function lspSection(): string {
  return "\n\n## Symbol navigation (lsp)\n" +
    "START code exploration here: lsp.overview on a file instead of reading it whole, " +
    "lsp.find to locate a symbol instead of an rg sweep, lsp.refs for callers instead of " +
    "grepping the name. These answer in symbols, not dumped text — far fewer tokens and " +
    "no false matches. Fall back to rg/read for non-code text, when a verb comes back " +
    "empty, or when lsp itself errors (language server missing or failing to start) — " +
    "a broken server is never a reason to stop the task; note it in one line and keep " +
    "working with rg/read. Verbs (await each; results are plain text; a symbol is a " +
    'name or dot path like "Gate.decide", and an ambiguous name errors with the ' +
    "candidates):\n" +
    "- lsp.find({pattern, path?}) — search symbols by name regex, optionally scoped " +
    "to a file or directory\n" +
    "- lsp.overview({path}) — every symbol in a file or directory\n" +
    "- lsp.show({symbol, context?}) — print a symbol's full definition body\n" +
    "- lsp.def({symbol}) — the declaration site\n" +
    "- lsp.refs({symbol, context?}) — all references across the workspace\n" +
    "- lsp.impls({symbol}) — implementations of an interface or abstract method\n" +
    "- lsp.calls({to}) or ({from}) — incoming/outgoing call hierarchy\n" +
    "- lsp.rename({symbol, new_name}) — rename across the codebase (edits files)\n" +
    "The first call in a session may take seconds (language-server startup + " +
    "indexing) — still worth it.";
}

/** Runs one leta invocation — injectable so tests need no binary. */
export type LetaRun = (
  args: string[],
  cwd: string,
) => Promise<{ code: number; stdout: string; stderr: string }>;

const defaultRun: LetaRun = async (args, cwd) => {
  const bin = letaBin();
  if (!bin) throw new Error("lsp backend unavailable: leta not found on PATH");
  const out = await new Deno.Command(bin, {
    args,
    cwd,
    // The daemon (and the language servers it spawns — node, tsserver, gopls…)
    // inherits this env; under launchd the bare PATH would strand them.
    env: { PATH: [Deno.env.get("PATH"), ...EXTRA_BIN_DIRS].filter(Boolean).join(":") },
    stdout: "piped",
    stderr: "piped",
  }).output();
  const decode = (b: Uint8Array) => new TextDecoder().decode(b);
  return { code: out.code, stdout: decode(out.stdout), stderr: decode(out.stderr) };
};

/**
 * The per-turn bridge behind the lsp.* host functions. Workspace registration
 * happens on the first call and is memoized for the turn; the daemon caches
 * language servers per workspace, so a later turn's first call finds the
 * language server already warm.
 */
export function createLspBridge(
  spawn: SpawnCtx,
  run: LetaRun = defaultRun,
): { call: (verb: string, args: unknown) => Promise<unknown> } {
  let ready: Promise<void> | undefined;
  const register = async (): Promise<void> => {
    // Point the daemon at this session's checkout (idempotent daemon-side).
    const res = await run(["workspace", "add"], spawn.workspace);
    if (res.code !== 0) {
      throw new Error(`lsp: leta workspace add failed: ${(res.stderr || res.stdout).trim()}`);
    }
  };
  return {
    call: async (verb: string, args: unknown): Promise<unknown> => {
      const build = VERBS[verb];
      if (!build) {
        throw new Error(`unknown lsp verb "${verb}" (has: ${Object.keys(VERBS).join(", ")})`);
      }
      const argv = build((args ?? {}) as Args);
      // Memoize the in-flight registration, but let a failed one be retried next call.
      ready ??= register().catch((e) => {
        ready = undefined;
        throw e;
      });
      await ready;
      const res = await run(argv, spawn.workspace);
      if (res.code !== 0) {
        throw new Error(
          `lsp.${verb} failed: ${(res.stderr || res.stdout).trim() || `exit ${res.code}`}`,
        );
      }
      return res.stdout;
    },
  };
}
