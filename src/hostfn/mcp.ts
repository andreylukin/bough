/**
 * The two MCP verbs a program can call: `mcp(server, tool, args)` and `mcpStatus()`.
 *
 * THE INVARIANT THIS HOLDS: **`mcpStatus()` cannot answer from a cache, and `mcp()`
 * cannot call a server the human did not grant.** Both are one line of code and both
 * are the whole point of this file.
 *
 * WHY STATUS MUST BE FRESH (plan §6.13, spec §10). `prompt/mcp-status.md` tells the
 * model, in the system prompt, to answer every MCP question from a fresh
 * `mcpStatus()` call and never from memory of an earlier turn. That instruction is
 * only worth giving if the fresh call is actually fresh: grants expire, a human
 * toggles a server between turns, another session restarts one, a child dies. So
 * nothing here memoizes — `mcpStatusFor` re-reads the registry file, re-resolves the
 * grant and re-reads the live connections on every call (`status.ts`), and this
 * module holds no state at all between calls. A one-turn cache would be invisible in
 * every test that calls status once and wrong in exactly the case the prompt is
 * about.
 *
 * WHY THE GRANT IS CHECKED HERE AND NOT ONLY IN THE CATALOG. The prompt's MCP
 * section lists the servers a turn may use, but a prompt is advice: the model can
 * name any string, and a program can loop over strings it read off disk. So the
 * grant is enforced at the call (`manager.requireGranted`), fresh, and a refusal
 * says what IS granted and that a human — not the program — is who grants more.
 *
 * WHY BOTH VERBS ARE BRIDGED TOGETHER. `mcpStatus()` is read-only and is bridged for
 * every turn; `mcp()` is the capability. They ship as one pair because a turn that
 * can call a server but cannot see its state has to guess at tool names, and a turn
 * that can see the state but not call anything is told about servers it cannot use.
 * The prompt gates each on its own name (`prompt/assemble.ts`), so the two halves of
 * every grant — the bridge and the prompt section — stay in step.
 *
 * ERRORS ARE THE PRODUCT SURFACE (spec §6). Every rejection here reaches the model as
 * a caught exception inside its program, so each names what failed, the state that
 * caused it, and the move that resolves it: an ungranted server names the granted
 * ones, a misspelled tool names the server's real tools, and a server that is down
 * says so with its own stderr rather than as "failed".
 */
import { McpError } from "../errors.ts";
import type { HostFns, TurnCtx } from "../types.ts";
import type { McpConfigOptions } from "../mcp/config.ts";
import { type McpManager, mcpManager, requireGranted, resolveGrant } from "../mcp/manager.ts";
import { type AuthLookup, mcpStatusFor } from "../mcp/status.ts";

export interface McpHostDeps {
  /** Absent = the process manager, which is what production wants. */
  manager?: McpManager;
  /** Registry location and `${VAR}` source. Absent = `~/.bough/mcp.json`. */
  config?: McpConfigOptions;
  /** Credential presence lookup. Absent = `oauth.ts`'s `hasTokens`. */
  auth?: AuthLookup;
  /** Injected clock, for grant expiry. Absent = `Date.now()`. */
  now?: () => number;
}

/**
 * Build the MCP host functions for one turn.
 *
 * The turn's ctx carries everything a call needs: which session is asking (the
 * grant), which checkout a spawned server should run in, and — for a subagent — the
 * grant it inherited from its spawner (`types.ts`, `mcp/manager.ts`).
 */
export function createMcpHostFns(
  ctx: TurnCtx,
  deps: McpHostDeps = {},
): Pick<HostFns, "mcp" | "mcpStatus"> {
  const manager = deps.manager ?? mcpManager();
  const config: McpConfigOptions = deps.config ?? manager.config;
  /**
   * Read PER CALL, never once per turn. `now` decides whether a TTL'd grant has
   * lapsed, and a clock sampled at turn start would keep a grant alive for the whole
   * turn after it expired — the failing-closed rule undone by a stale variable.
   */
  const opts = () => ({ ...config, ...(deps.now ? { now: deps.now() } : {}) });

  return {
    mcp: async (server: string, tool: string, argsJson: string): Promise<string> => {
      let args: unknown;
      try {
        args = argsJson === "" ? {} : JSON.parse(argsJson);
      } catch {
        throw new McpError(
          400,
          `mcp("${server}", "${tool}", …): the arguments were not valid JSON. Pass a ` +
            `plain object matching the tool's parameters.`,
        );
      }
      // Fresh on every call: a grant revoked between two calls in the SAME program
      // is gone from the second one.
      requireGranted(ctx, server, opts());
      const result = await manager.call(ctx.sessionId, server, tool, args, {
        workspace: ctx.workspace,
      });
      // The bridge is string-only and the worker re-inflates with JSON.parse, so a
      // tool that returned nothing must still come back as valid JSON.
      return JSON.stringify(result ?? null);
    },

    mcpStatus: (): Promise<string> => {
      const now = opts();
      return Promise.resolve(JSON.stringify(mcpStatusFor({
        ...now,
        sessionId: ctx.sessionId,
        // The effective grant, resolved through the same function the call path
        // uses, so status and enforcement cannot disagree about what this turn may
        // call. Disagreement would produce a model told it has no servers while
        // `mcp()` works, or told it has one that then refuses.
        grant: resolveGrant(ctx, now),
        manager,
        ...(deps.auth ? { auth: deps.auth } : {}),
      })));
    },
  };
}
