/**
 * The host side of the code-mode VM. Each supervisor round is one JavaScript program
 * executed in a fresh Deno Worker with `permissions: "inherit"` — the program runs as
 * the user, with the user's full authority: filesystem, network, env, subprocesses,
 * npm/jsr imports. Nothing here is a security boundary.
 *
 * The host functions bridged in are therefore convenience and integration, not
 * confinement. They earn their place by carrying harness behavior a raw Deno call
 * cannot: bash() dies on the turn's interrupt, auto-backgrounds past 60s and digests
 * oversized output; write()/edit() feed the done-gate's write signal; agent(), ask()
 * and state() reach the session's DB and TUI.
 *
 * A program is disposable compute: fresh isolate per round, hard wall-clock timeout,
 * wound down (children killed, then terminated) on overrun or interrupt.
 * Host-function failures reject inside the program as ordinary exceptions the
 * supervisor's code may catch.
 */

import { checkSyntax } from "../text.ts";

/**
 * The program's parameter names, mirroring vm_worker.ts's own AsyncFunction call.
 * Duplicated rather than imported to keep the worker module self-contained;
 * vm.test.ts pins the two lists equal.
 * They must match: a program that shadows a host name (`let bash = 1`) is a
 * SyntaxError, and the pre-flight check should agree with the worker about it.
 */
export const PROGRAM_PARAMS = [
  "bash",
  "sh",
  "extract",
  "fetch",
  "bashBg",
  "bashOutput",
  "bashWait",
  "bashKill",
  "read",
  "write",
  "edit",
  "agent",
  "spawn",
  "join",
  "adopt",
  "ask",
  "mcp",
  "mcpStatus",
  "lsp",
  "artifact",
  "recall",
  "image",
  "schedule",
  "state",
  "workflow",
  "console",
];

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
   * Recurring runs (schedules.ts): one verb-dispatched function like lsp() —
   * args in and result back travel as JSON; the worker side exposes it as the
   * `schedule.*` method object (list/add/enable/disable/remove).
   */
  schedule?(verb: string, argsJson: string): Promise<string>;
  /**
   * Durable per-conversation key/value notes (state.ts): one verb-dispatched
   * function like schedule() — args in and result back travel as JSON; the worker
   * side exposes it as the `state.*` method object (get/set/list/delete). Bridged
   * for every supervisor turn.
   */
  state?(verb: string, argsJson: string): Promise<string>;
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
 * How long a stopping program gets to kill the processes it spawned before the
 * worker is terminated regardless. Long enough for a SIGTERM sweep, short enough
 * that the stop button still feels instant.
 */
const ABORT_GRACE_MS = 1_000;

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
  // Parse before spawning: a program that cannot compile used to reach the model
  // as a bare SyntaxError over ten frames of Deno internals, with no line and no
  // source — nothing it could act on, so it burned the turn guessing. The worker
  // parses it again for real; this pass exists only to say WHERE.
  const bad = checkSyntax(code, PROGRAM_PARAMS, "program");
  if (bad) return Promise.resolve({ ok: false, logs: [], error: bad.message });

  const worker = new Worker(new URL("./vm_worker.ts", import.meta.url).href, {
    type: "module",
    // The program runs with everything the server itself has: filesystem, network,
    // env, subprocesses, npm/jsr imports. The host functions below are convenience
    // and integration (they carry the turn's interrupt, the auto-background, the
    // output digest, and the session's DB/TUI-backed verbs) — they are NOT a
    // boundary, and a program is free to reach past them to raw Deno APIs.
    deno: { permissions: "inherit" },
  });

  return new Promise<ProgramResult>((resolve) => {
    let settled = false;
    /** Set while a stop is in flight: the worker's "aborted" ack calls it. */
    let onAborted: (() => void) | undefined;
    // Console lines already streamed out of the worker — an interrupt terminates
    // the worker before it can post its batched logs, so this copy is what keeps
    // the partial output in the tool result.
    const streamed: string[] = [];
    const interrupted = (): ProgramResult => ({
      ok: false,
      logs: streamed,
      error: "program interrupted by the user",
    });
    /**
     * Stopping the program is a handshake, not just a terminate(). The program runs
     * with real permissions, so it may have spawned processes of its own — and those
     * are children of THIS server process, which worker.terminate() leaves running
     * forever. So: ask the worker to kill what it spawned, wait briefly for its ack,
     * then terminate. A worker wedged in a synchronous loop can't answer, hence the
     * grace timeout — it stops the turn on schedule either way.
     */
    const stop = (result: ProgramResult) => {
      if (settled) return;
      let acked = false;
      const done = () => {
        if (acked) return;
        acked = true;
        clearTimeout(grace);
        finish(result);
      };
      onAborted = done;
      const grace = setTimeout(done, ABORT_GRACE_MS);
      try {
        worker.postMessage({ type: "abort" });
      } catch {
        done(); // worker already gone — nothing to wind down
      }
    };
    const onAbort = () => stop(interrupted());
    const finish = (result: ProgramResult) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      worker.terminate();
      resolve(result);
    };

    // A timed-out program gets the same wind-down as an interrupted one: whatever
    // it spawned is killed before the worker goes away.
    const timer = setTimeout(
      () => stop({ ok: false, logs: streamed, error: `program timed out after ${timeoutMs}ms` }),
      timeoutMs,
    );
    // Already stopped before we started: the program never ran, so there is nothing
    // to wind down and no reason to wait on an ack.
    if (signal?.aborted) return finish(interrupted());
    signal?.addEventListener("abort", onAbort, { once: true });

    worker.onmessage = async (e: MessageEvent) => {
      const msg = e.data as
        | { type: "host"; id: number; fn: keyof HostFns; args: unknown[] }
        | { type: "log"; line: string }
        | { type: "aborted" }
        | { type: "done"; logs: string[] }
        | { type: "error"; message: string; logs: string[] };
      // The worker finished killing what it spawned — stop waiting on the grace timer.
      if (msg.type === "aborted") return onAborted?.();
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
