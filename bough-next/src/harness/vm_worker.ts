/// <reference no-default-lib="true" />
/// <reference lib="deno.worker" />
/**
 * The sandbox side of the code-mode VM (SPEC.md §5.2, V8 edition): this module runs
 * as a Deno Worker with `permissions: "none"` — its V8 isolate can touch nothing on
 * the host. The four host functions (bash/read/write/edit) are the entire capability
 * surface, bridged to the main process over postMessage; everything else (fs, net,
 * env, Deno APIs) is denied by the runtime.
 *
 * Protocol (see vm.ts):
 *   main → worker  {type:"run", code}
 *   worker → main  {type:"host", id, fn, args}          host-function call
 *   main → worker  {type:"host_result", id, ok, value}  its result / error
 *   worker → main  {type:"done", logs} | {type:"error", message, logs}
 */

type HostName = "bash" | "read" | "write" | "edit";

const pending = new Map<number, { resolve: (v: string) => void; reject: (e: Error) => void }>();
let seq = 0;
const logs: string[] = [];

function hostCall(fn: HostName, args: unknown[]): Promise<string> {
  const id = ++seq;
  const p = new Promise<string>((resolve, reject) => pending.set(id, { resolve, reject }));
  self.postMessage({ type: "host", id, fn, args });
  return p;
}

function show(v: unknown): string {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

const sandboxConsole = {
  log: (...args: unknown[]) => logs.push(args.map(show).join(" ")),
  error: (...args: unknown[]) => logs.push(args.map(show).join(" ")),
  warn: (...args: unknown[]) => logs.push(args.map(show).join(" ")),
  info: (...args: unknown[]) => logs.push(args.map(show).join(" ")),
};

async function run(code: string): Promise<void> {
  const bash = (cmd: string) => hostCall("bash", [cmd]);
  const read = (path: string) => hostCall("read", [path]);
  const write = (path: string, content: string) => hostCall("write", [path, content]);
  const edit = (path: string, oldText: string, newText: string) =>
    hostCall("edit", [path, oldText, newText]);

  // deno-lint-ignore no-explicit-any
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor as any;
  const program = new AsyncFunction("bash", "read", "write", "edit", "console", code);
  await program(bash, read, write, edit, sandboxConsole);
}

self.onmessage = (e: MessageEvent) => {
  const msg = e.data as
    | { type: "run"; code: string }
    | { type: "host_result"; id: number; ok: boolean; value: string };
  if (msg.type === "host_result") {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    if (!p) return;
    if (msg.ok) p.resolve(msg.value);
    else p.reject(new Error(msg.value));
    return;
  }
  run(msg.code)
    .then(() => self.postMessage({ type: "done", logs }))
    .catch((err) =>
      self.postMessage({
        type: "error",
        message: String((err as Error)?.stack ?? err),
        logs,
      })
    );
};
