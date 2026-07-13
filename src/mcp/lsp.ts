/**
 * First-class LSP verbs over the MCP layer: symbol navigation is a core
 * capability like read/bash, so it is bridged into every supervisor program as
 * `lsp.*` host functions instead of hiding behind a skill grant — but ONLY the
 * curated verbs below, with bough-owned names, so the model-facing surface stays
 * stable if the backing server changes.
 *
 * The backing server is the registry entry named "serena" — shipped as a builtin
 * (config.ts BUILTIN_SERVERS), so lsp.* exists on every install; a user entry in
 * ~/.bough/mcp/servers.json overrides the launch command. Everything stays inside
 * the existing trust model: connections go through the McpManager (seatbelt spawn,
 * per-session egress), and every underlying tool call passes the Claw Patrol gate
 * exactly like an mcp() call.
 *
 * Lazy by design: nothing is spawned at turn start. The first lsp.* call connects
 * the server and activates the session workspace as the project; later calls (and
 * later turns — connections are cached per session) reuse the warm language server.
 */
import { loadRegistry } from "./config.ts";
import type { ServerCatalog, SpawnCtx } from "./manager.ts";

/** The registry name the LSP verbs are backed by. */
export const LSP_SERVER = "serena";

/** verb → serena tool. Curated: navigation + diagnostics + rename, nothing else —
 * serena's file/shell/memory tools stay behind an explicit mcp() grant. */
const VERBS: Record<string, string> = {
  find: "find_symbol",
  def: "find_declaration",
  refs: "find_referencing_symbols",
  impls: "find_implementations",
  overview: "get_symbols_overview",
  diagnostics: "get_diagnostics_for_file",
  rename: "rename_symbol",
};

/** The manager surface the bridge needs (McpManager satisfies it; tests fake it). */
export interface LspManager {
  ensure(sessionId: string, servers: string[], spawn: SpawnCtx): Promise<ServerCatalog[]>;
  call(sessionId: string, server: string, tool: string, args: unknown): Promise<unknown>;
}

/** True when the backing server is registered — the turn runner's bridge/prompt gate. */
export function lspAvailable(): boolean {
  return LSP_SERVER in loadRegistry().servers;
}

/** The system-prompt section for turns that have lsp.* bridged. */
export function lspSection(): string {
  return "\n\n## Symbol navigation (lsp)\n" +
    "START code exploration here: lsp.overview on a file instead of reading it whole, " +
    "lsp.find to locate a symbol instead of an rg sweep, lsp.refs for callers instead of " +
    "grepping the name. These answer in symbols, not dumped text — far fewer tokens and " +
    "no false matches. Fall back to rg/read for non-code text, when a verb comes back " +
    "empty, or when lsp itself errors (language server missing or failing to start) — " +
    "a broken server is never a reason to stop the task; note it in one line and keep " +
    "working with rg/read. Verbs (await each; the args object goes to the language " +
    "backend verbatim):\n" +
    "- lsp.find({name_path_pattern, relative_path?, include_body?}) — search symbols by " +
    'name path ("method" matches anywhere, "Class/method" scoped, substring via ' +
    "substring_matching: true)\n" +
    "- lsp.def({relative_path, regex}) — declaration of the symbol whose usage in that " +
    "file matches regex\n" +
    "- lsp.refs({name_path, relative_path}) — symbols referencing the given symbol\n" +
    "- lsp.impls({name_path, relative_path}) — implementations of the given symbol\n" +
    "- lsp.overview({relative_path}) — top-level symbols in a file\n" +
    "- lsp.diagnostics({relative_path, start_line?, end_line?}) — language-server " +
    "diagnostics for a file\n" +
    "- lsp.rename({name_path, relative_path, new_name}) — rename across the codebase\n" +
    'name_path addresses a symbol (e.g. "UserHandler/get_user"); relative_path is ' +
    "workspace-relative and pins the symbol's defining file. The first call in a " +
    "session may take seconds (language-server startup + indexing) — still worth it.";
}

/**
 * The per-turn bridge behind the lsp.* host functions. Connect + project
 * activation happen on the first call and are memoized for the turn; the
 * connection itself is cached per session by the manager, so a later turn's
 * first call finds the language server already warm.
 */
export function createLspBridge(
  sessionId: string,
  spawn: SpawnCtx,
  manager: LspManager,
): { call: (verb: string, args: unknown) => Promise<unknown> } {
  let ready: Promise<void> | undefined;
  const connect = async (): Promise<void> => {
    const [catalog] = await manager.ensure(sessionId, [LSP_SERVER], spawn);
    if (catalog.error) {
      throw new Error(`lsp backend "${LSP_SERVER}" unavailable: ${catalog.error}`);
    }
    // Point the language server at this session's checkout (idempotent server-side).
    await manager.call(sessionId, LSP_SERVER, "activate_project", { project: spawn.workspace });
  };
  return {
    call: async (verb: string, args: unknown): Promise<unknown> => {
      const tool = VERBS[verb];
      if (!tool) {
        throw new Error(`unknown lsp verb "${verb}" (has: ${Object.keys(VERBS).join(", ")})`);
      }
      // Memoize the in-flight connect, but let a failed one be retried next call.
      ready ??= connect().catch((e) => {
        ready = undefined;
        throw e;
      });
      await ready;
      return await manager.call(sessionId, LSP_SERVER, tool, args);
    },
  };
}
