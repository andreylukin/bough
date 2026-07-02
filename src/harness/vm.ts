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
}

export interface ProgramResult {
  ok: boolean;
  /** console.log/error/warn/info output, in order. */
  logs: string[];
  /** Present when ok=false: the thrown error (with stack) or the timeout notice. */
  error?: string;
}

const DEFAULT_TIMEOUT_MS = 180_000;

/** Run one supervisor program in a sealed V8 isolate with the given host functions. */
export function runProgram(
  code: string,
  host: HostFns,
  timeoutMs: number = DEFAULT_TIMEOUT_MS,
): Promise<ProgramResult> {
  const worker = new Worker(new URL("./vm_worker.ts", import.meta.url).href, {
    type: "module",
    deno: { permissions: "none" },
  });

  return new Promise<ProgramResult>((resolve) => {
    let settled = false;
    const finish = (result: ProgramResult) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      worker.terminate();
      resolve(result);
    };

    const timer = setTimeout(
      () => finish({ ok: false, logs: [], error: `program timed out after ${timeoutMs}ms` }),
      timeoutMs,
    );

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
