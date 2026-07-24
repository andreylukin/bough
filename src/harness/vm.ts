/**
 * The host side of the code-mode VM. Each supervisor round is one JavaScript program executed in a fresh
 * Deno Worker with `permissions: "none"`: an isolated V8 heap that cannot reach the
 * filesystem, network, env, or Deno APIs. The ONLY capabilities are the async host
 * functions we bridge in — and those run here on the host, where the real tool
 * implementations enforce workspace confinement and sandboxing (the session VM).
 *
 * A program is disposable compute: fresh isolate per round, hard wall-clock timeout,
 * terminated (not awaited) on overrun. Host-function failures reject inside the
 * program as ordinary exceptions the supervisor's code may catch.
 */

export interface HostFns {
  bash(cmd: string): Promise<string>;
  /**
   * Background shells: bashBg spawns a command that outlives the program and the
   * turn, returning {id, pid} as JSON; bashOutput returns output accrued since the
   * last call plus a status line; bashKill terminates one. Always wired by
   * run_steps, like bash.
   */
  /**
   * Concurrent shells (bash.ts shConcurrent): the commands travel in as a JSON
   * array and the [{code, out}, …] results come back as JSON — the worker side
   * exposes it as the variadic sh(...cmds). Always wired by run_steps, like bash.
   */
  sh(cmdsJson: string): Promise<string>;
  /**
   * Cheap-model extraction over text the program already holds (worker/extract.ts):
   * the optional JSON Schema travels in as JSON and the result comes back as JSON
   * — the worker side exposes it as extract(text, instruction, schema?). Always
   * wired by run_steps, like bash.
   */
  extract(text: string, instruction: string, schemaJson: string): Promise<string>;
  /**
   * HTTP from the host (tools/fetch_url.ts): the worker isolate has no network of
   * its own, so this is the program's only egress. Options travel in as JSON and
   * the {status, ok, url, contentType, body, truncated} result comes back as JSON.
   * Always wired by run_steps, like bash.
   */
  fetch(url: string, optsJson: string): Promise<string>;
  bashBg(cmd: string): Promise<string>;
  bashOutput(id: string): Promise<string>;
  bashWait(id: string): Promise<string>;
  bashKill(id: string): Promise<string>;
  read(path: string): Promise<string>;
  write(path: string, content: string): Promise<string>;
  edit(path: string, oldText: string, newText: string): Promise<string>;
  /**
   * Delegation, bridged only for sessions that may spawn (run_steps wires them from
   * ToolRunCtx). `agent` (blocking result), `spawn` (background handle) and `join`
   * (await a background subagent) return JSON — the worker side parses it back into
   * an object; the string keeps the postMessage protocol string-only.
   */
  agent?(task: string): Promise<string>;
  spawn?(task: string): Promise<string>;
  join?(sessionId: string): Promise<string>;
  adopt?(sessionId: string): Promise<string>;
  /**
   * Ask the HUMAN a mid-task question (bridged for supervisor turns — asks.ts):
   * the program parks until they answer in the TUI. Options travel in as JSON
   * ({options?: string[]}); the chosen/typed answer returns as a plain string.
   * Rejects (catchably) when the user declines, and on turn interrupt.
   */
  ask?(question: string, optsJson: string): Promise<string>;
  /**
   * MCP tool call (bridged only for turns granted servers): args travel in as JSON
   * and the result returns as JSON — the worker side parses both ends.
   */
  mcp?(server: string, tool: string, argsJson: string): Promise<string>;
  /**
   * MCP management state for the session (registry/auth/active/connections) as
   * JSON. Read-only and always bridged for supervisor turns — status is not a
   * capability grant; calling tools still requires mcp().
   */
  mcpStatus?(): Promise<string>;
  /**
   * LSP symbol verb (bridged when a language-intelligence server is registered —
   * mcp/lsp.ts): args in and result back travel as JSON, like mcp(). The worker
   * side exposes it as the `lsp.*` method object.
   */
  lsp?(verb: string, argsJson: string): Promise<string>;
  /**
   * Publish an artifact for browser viewing (server/artifacts.ts): write `content`
   * to the session's artifact store, host it on the bough server, and return the
   * artifact ({url, href, …}) as JSON — the worker side parses it back. Bridged for
   * every supervisor turn.
   */
  artifact?(name: string, content: string): Promise<string>;
  /**
   * Semantic search over all past conversations (recall.ts): the RecallResult
   * ({hits, indexed}) returns as JSON — the worker side parses it back. Bridged
   * for every supervisor turn.
   */
  recall?(query: string, k?: number): Promise<string>;
  /**
   * Show an image file to the model (turn.ts): the file is copied into the
   * attachment store and posted as a system note carrying the picture. Returns a
   * plain confirmation line (no JSON) — the image itself never crosses the
   * postMessage bridge. Bridged for every supervisor turn.
   */
  image?(path: string, note?: string): Promise<string>;
  /**
   * Ship the session's work into the origin repo as a real commit (+ optional
   * push) — vcs/agentdiff.ts shipToOrigin. Options in and ShipResult back travel
   * as JSON. Bridged only for root-session turns with a repo workspace.
   */
  ship?(optsJson: string): Promise<string>;
  /**
   * Export the session's work into a git branch and open a GitHub PR — vcs/agentdiff.ts
   * openPr. Options in and PrResult back travel as JSON. Bridged alongside ship for
   * root-session repo turns.
   */
  pr?(optsJson: string): Promise<string>;
  /**
   * Recurring runs (schedules.ts): one verb-dispatched function like lsp() —
   * args in and result back travel as JSON; the worker side exposes it as the
   * `schedule.*` method object (list/add/enable/disable/remove).
   */
  schedule?(verb: string, argsJson: string): Promise<string>;
  /**
   * Workflows (workflow.ts): scripted multi-agent orchestration, verb-dispatched
   * like schedule() — the worker side exposes it as the `workflow.*` method
   * object (start/rerun/stop/pause/resume/status/list). Bridged only for
   * root-session turns that may delegate.
   */
  workflow?(verb: string, argsJson: string): Promise<string>;
}

export interface ProgramResult {
  ok: boolean;
  /** console.log/error/warn/info output, in order. */
  logs: string[];
  /** Present when ok=false: the thrown error (with stack) or the timeout notice. */
  error?: string;
}

const DEFAULT_TIMEOUT_MS = 180_000;

/**
 * Run one supervisor program in a sealed V8 isolate with the given host functions.
 * `signal` (the turn's interrupt) terminates the worker mid-program — host functions
 * already in flight are expected to observe the same signal and die on their own
 * (bash kills its child process).
 */
export function runProgram(
  code: string,
  host: HostFns,
  timeoutMs: number = DEFAULT_TIMEOUT_MS,
  signal?: AbortSignal,
  /** Fires for each console.* line as the program prints it (display-only
   * streaming — the batched `logs` in the result are unaffected). */
  onLog?: (line: string) => void,
): Promise<ProgramResult> {
  const worker = new Worker(new URL("./vm_worker.ts", import.meta.url).href, {
    type: "module",
    deno: { permissions: "none" },
  });

  return new Promise<ProgramResult>((resolve) => {
    let settled = false;
    // Console lines already streamed out of the worker — an interrupt terminates
    // the worker before it can post its batched logs, so this copy is what keeps
    // the partial output in the tool result.
    const streamed: string[] = [];
    const onAbort = () =>
      finish({ ok: false, logs: streamed, error: "program interrupted by the user" });
    const finish = (result: ProgramResult) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      worker.terminate();
      resolve(result);
    };

    const timer = setTimeout(
      () => finish({ ok: false, logs: [], error: `program timed out after ${timeoutMs}ms` }),
      timeoutMs,
    );
    if (signal?.aborted) return onAbort();
    signal?.addEventListener("abort", onAbort, { once: true });

    worker.onmessage = async (e: MessageEvent) => {
      const msg = e.data as
        | { type: "host"; id: number; fn: keyof HostFns; args: unknown[] }
        | { type: "log"; line: string }
        | { type: "done"; logs: string[] }
        | { type: "error"; message: string; logs: string[] };
      if (msg.type === "log") {
        streamed.push(msg.line);
        onLog?.(msg.line);
        return;
      }
      if (msg.type === "done") return finish({ ok: true, logs: msg.logs });
      if (msg.type === "error") return finish({ ok: false, logs: msg.logs, error: msg.message });
      // Host-function call: run it here, send the result (or the error) back in.
      try {
        const fn = host[msg.fn];
        if (!fn) throw new Error(`unknown host function: ${msg.fn}`);
        // deno-lint-ignore no-explicit-any
        const value = await (fn as any)(...msg.args);
        worker.postMessage({ type: "host_result", id: msg.id, ok: true, value: String(value) });
      } catch (err) {
        worker.postMessage({
          type: "host_result",
          id: msg.id,
          ok: false,
          value: (err as Error).message ?? String(err),
        });
      }
    };

    worker.onerror = (e) => {
      e.preventDefault();
      finish({ ok: false, logs: [], error: `worker error: ${e.message}` });
    };

    worker.postMessage({ type: "run", code });
  });
}
