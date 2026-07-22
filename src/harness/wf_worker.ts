/// <reference no-default-lib="true" />
/// <reference lib="deno.worker" />
/**
 * The sandbox side of the workflow VM: runs one workflow script body (meta already
 * stripped host-side) as a Deno Worker with `permissions: "none"`. The script's
 * capability surface is deliberately tiny — agent() bridges to a real subagent
 * turn on the host; phase()/log() are progress reporting; parallel()/pipeline()
 * are pure in-worker combinators.
 *
 * Protocol (see workflow.ts):
 *   main → worker  {type:"run", code, args}
 *   worker → main  {type:"host", id, fn, args}          agent/phase/log call
 *   main → worker  {type:"host_result", id, ok, value}  its result / error
 *   worker → main  {type:"done", result} | {type:"error", message}
 */

const pending = new Map<number, { resolve: (v: string) => void; reject: (e: Error) => void }>();
let seq = 0;

function hostCall(fn: "agent" | "phase" | "log", args: unknown[]): Promise<string> {
  const id = ++seq;
  const p = new Promise<string>((resolve, reject) => pending.set(id, { resolve, reject }));
  self.postMessage({ type: "host", id, fn, args });
  return p;
}

// Same guard as vm_worker.ts: process.exit()/Deno.exit() would kill the worker
// silently and strand the run until its wall timeout.
const exitTrap = (code?: unknown) => {
  throw new Error(`exit(${code ?? 0}) is not available in a workflow — end by returning`);
};
try {
  const g = globalThis as { process?: { exit?: unknown }; Deno?: { exit?: unknown } };
  if (g.process) g.process.exit = exitTrap;
  if (g.Deno) g.Deno.exit = exitTrap;
} catch { /* frozen globals — nothing to guard */ }

async function run(code: string, args: unknown): Promise<unknown> {
  // agent(prompt, opts?) → the subagent's report text. Throws on agent failure so
  // parallel() maps the slot to null and pipeline() drops the item.
  const agent = async (prompt: string, opts?: Record<string, unknown>) => {
    const out = JSON.parse(await hostCall("agent", [JSON.stringify({ ...opts ?? {}, prompt })]));
    return out as string | null;
  };
  // phase/log: fire-and-forget progress — a workflow never blocks on display.
  const phase = (title: string) => void hostCall("phase", [String(title)]).catch(() => {});
  const log = (message: unknown) => void hostCall("log", [show(message)]).catch(() => {});
  // parallel(thunks): run all concurrently; a thunk that throws resolves to null
  // so one failure never rejects the whole barrier.
  const parallel = (thunks: Array<() => unknown>) =>
    Promise.all(thunks.map((t) => Promise.resolve().then(t).catch(() => null)));
  // pipeline(items, ...stages): each item flows through all stages independently —
  // no barrier between stages. A stage that throws drops the item to null and
  // skips its remaining stages. Stage callbacks get (prev, originalItem, index).
  const pipeline = (items: unknown[], ...stages: Array<(v: unknown, item: unknown, i: number) => unknown>) =>
    Promise.all(items.map(async (item, i) => {
      let v: unknown = item;
      try {
        for (const stage of stages) v = await stage(v, item, i);
        return v;
      } catch {
        return null;
      }
    }));

  // deno-lint-ignore no-explicit-any
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor as any;
  const program = new AsyncFunction(
    "agent",
    "parallel",
    "pipeline",
    "phase",
    "log",
    "args",
    "console",
    code,
  );
  return await program(agent, parallel, pipeline, phase, log, args, {
    log,
    error: log,
    warn: log,
    info: log,
  });
}

function show(v: unknown): string {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

self.onmessage = (e: MessageEvent) => {
  const msg = e.data as
    | { type: "run"; code: string; args: unknown }
    | { type: "host_result"; id: number; ok: boolean; value: string };
  if (msg.type === "host_result") {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    if (!p) return;
    if (msg.ok) p.resolve(msg.value);
    else p.reject(new Error(msg.value));
    return;
  }
  run(msg.code, msg.args)
    .then((result) => self.postMessage({ type: "done", result: JSON.stringify(result ?? null) }))
    .catch((err) =>
      self.postMessage({ type: "error", message: String((err as Error)?.stack ?? err) })
    );
};
