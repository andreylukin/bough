/**
 * The host side of the code-mode VM. Each supervisor round is one JavaScript program executed in a fresh
 * Deno Worker with `permissions: "none"`: an isolated V8 heap that cannot reach the
 * filesystem, network, env, or Deno APIs. The ONLY capabilities are the async host
 * functions we bridge in — and those run here on the host, where the real tool
 * implementations enforce workspace confinement and the Seatbelt sandbox.
 *
 * A program is disposable compute: fresh isolate per round, hard wall-clock timeout,
 * terminated (not awaited) on overrun. Host-function failures reject inside the
 * program as ordinary exceptions the supervisor's code may catch.
 */

export interface HostFns {
  bash(cmd: string): Promise<string>;
  read(path: string): Promise<string>;
  write(path: string, content: string): Promise<string>;
  edit(path: string, oldText: string, newText: string): Promise<string>;
  /**
   * One delegated verifiable unit on the local-worker ladder (worker/ladder.ts).
   * Returns the UnitResult as JSON — string-only protocol, like agent().
   */
  worker?(instruction: string, check: string): Promise<string>;
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
): Promise<ProgramResult> {
  const worker = new Worker(new URL("./vm_worker.ts", import.meta.url).href, {
    type: "module",
    deno: { permissions: "none" },
  });

  return new Promise<ProgramResult>((resolve) => {
    let settled = false;
    const onAbort = () => finish({ ok: false, logs: [], error: "program interrupted by the user" });
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
        | { type: "done"; logs: string[] }
        | { type: "error"; message: string; logs: string[] };
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
