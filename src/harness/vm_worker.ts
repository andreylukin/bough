/// <reference no-default-lib="true" />
/// <reference lib="deno.worker" />
/**
 * The sandbox side of the code-mode VM: this module runs
 * as a Deno Worker with `permissions: "none"` — its V8 isolate can touch nothing on
 * the host. The four host functions (bash/read/write/edit) are the entire capability
 * surface, bridged to the main process over postMessage; everything else (fs, net,
 * env, Deno APIs) is denied by the runtime.
 *
 * Protocol (see vm.ts):
 *   main → worker  {type:"run", code}
 *   worker → main  {type:"host", id, fn, args}          host-function call
 *   main → worker  {type:"host_result", id, ok, value}  its result / error
 *   worker → main  {type:"log", line}                     one console.* line, as printed
 *   worker → main  {type:"done", logs} | {type:"error", message, logs}
 *
 * console lines are BOTH streamed ({type:"log"} — the TUI renders them live) and
 * batched into `logs` (the model still receives the full output in the tool
 * result). Display-only streaming: context contents are unchanged.
 */

type HostName =
  | "bash"
  | "sh"
  | "extract"
  | "fetch"
  | "bashBg"
  | "bashOutput"
  | "bashWait"
  | "bashKill"
  | "read"
  | "write"
  | "edit"
  | "agent"
  | "spawn"
  | "join"
  | "adopt"
  | "ask"
  | "mcp"
  | "mcpStatus"
  | "lsp"
  | "artifact"
  | "recall"
  | "image"
  | "ship"
  | "pr"
  | "schedule"
  | "state"
  | "workflow";

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

// A console.* call emits its line immediately (live progress in the TUI) AND
// keeps it in the batch (the model-facing tool result ships the joined logs).
const print = (...args: unknown[]) => {
  const line = args.map(show).join(" ");
  logs.push(line);
  self.postMessage({ type: "log", line });
};
const sandboxConsole = { log: print, error: print, warn: print, info: print };

// Node-ism guard: Deno exposes a `process` global even in permissions-none
// workers, and process.exit()/Deno.exit() TERMINATE THE WORKER SILENTLY — the
// host's runProgram promise never settles, freezing the turn until its wall
// timeout (45 min for delegating turns; bench trials burned 900s each on
// exactly this). Weak models emit `process.exit(1)` as an "assertion failed"
// idiom, so make it throw a catchable error the round can report instead.
const exitTrap = (code?: unknown) => {
  throw new Error(
    `exit(${code ?? 0}) is not available in this sandbox — a program ends by ` +
      `returning; throw an Error to signal failure`,
  );
};
try {
  const g = globalThis as { process?: { exit?: unknown }; Deno?: { exit?: unknown } };
  if (g.process) g.process.exit = exitTrap;
  if (g.Deno) g.Deno.exit = exitTrap;
} catch { /* frozen globals — nothing to guard */ }

async function run(code: string): Promise<void> {
  const bash = (cmd: string) => hostCall("bash", [cmd]);
  // Concurrent shells: the commands ride out as a JSON array and the results come
  // back as JSON [{code, out}, …] — a non-zero code is data here, never a throw.
  const sh = async (...cmds: string[]) => JSON.parse(await hostCall("sh", [JSON.stringify(cmds)]));
  // Cheap-model extraction: the optional JSON Schema rides out as JSON and the
  // result (a string, or the schema-shaped object) comes back as JSON. Rejects
  // catchably when no worker is reachable — read the text yourself then.
  const extract = async (text: string, instruction: string, schema?: unknown) =>
    JSON.parse(await hostCall("extract", [text, instruction, JSON.stringify(schema ?? null)]));
  // HTTP: the worker isolate has no network, so this hops to the host. Options ride
  // out as JSON and the response object comes back as JSON. Rejects catchably on a
  // transport failure, a non-http(s) URL, the 30s deadline, or an interrupt.
  const fetch = async (url: string, opts?: unknown) =>
    JSON.parse(await hostCall("fetch", [url, JSON.stringify(opts ?? {})]));
  // Background shells: the spawn handle comes back as JSON ({id, pid} — the
  // postMessage protocol stays string-only); output/kill return plain text.
  const bashBg = async (cmd: string) => JSON.parse(await hostCall("bashBg", [cmd]));
  const bashOutput = (id: string) => hostCall("bashOutput", [id]);
  const bashWait = (id: string) => hostCall("bashWait", [id]);
  const bashKill = (id: string) => hostCall("bashKill", [id]);
  const read = (path: string) => hostCall("read", [path]);
  const write = (path: string, content: string) => hostCall("write", [path, content]);
  const edit = (path: string, oldText: string, newText: string) =>
    hostCall("edit", [path, oldText, newText]);
  // Delegation: the host sends subagent results/handles as JSON (postMessage stays
  // string-only); parse them back so the program gets real objects. Sessions that
  // may not delegate have no bridged fn — the call rejects as "unknown host function".
  const agent = async (task: string) => JSON.parse(await hostCall("agent", [task]));
  const spawn = async (task: string) => JSON.parse(await hostCall("spawn", [task]));
  const join = async (sessionId: string) => JSON.parse(await hostCall("join", [sessionId]));
  const adopt = (sessionId: string) => hostCall("adopt", [sessionId]);
  // Ask the human: options ride out as JSON (string-only protocol); the answer
  // comes back as a plain string. Rejects on decline/interrupt (catchable).
  const ask = (question: string, opts?: unknown) =>
    hostCall("ask", [question, JSON.stringify(opts ?? {})]);
  // MCP: args out and result back both travel as JSON (string-only protocol).
  // Turns without granted servers have no bridged fn — the call rejects.
  const mcp = async (server: string, tool: string, args?: unknown) =>
    JSON.parse(await hostCall("mcp", [server, tool, JSON.stringify(args ?? {})]));
  // MCP management state (registry/auth/active/connections) — read-only, always on.
  const mcpStatus = async () => JSON.parse(await hostCall("mcpStatus", []));
  // LSP symbol verbs (bridged only when a language backend is registered): one
  // host function fanned out as a method object; JSON round-trip like mcp().
  const lspCall = async (verb: string, args?: unknown) =>
    JSON.parse(await hostCall("lsp", [verb, JSON.stringify(args ?? {})]));
  const lsp = Object.fromEntries(
    ["find", "show", "def", "refs", "impls", "calls", "overview", "rename"].map(
      (verb) => [verb, (args?: unknown) => lspCall(verb, args)],
    ),
  );
  // Artifacts: write a file to the session's artifact store and host it; returns the
  // artifact object ({url, href, …}). JSON round-trip like agent()/mcp(). A non-string
  // content (a *.ui.json spec object) is stringified so programs can pass it directly.
  const artifact = async (name: string, content: unknown) =>
    JSON.parse(
      await hostCall("artifact", [
        name,
        typeof content === "string" ? content : JSON.stringify(content),
      ]),
    );
  // Recall: semantic search over past conversations; returns {hits, indexed}.
  const recall = async (query: string, k?: number) =>
    JSON.parse(await hostCall("recall", k === undefined ? [query] : [query, k]));
  // Show an image to the model: the host copies the file into the attachment
  // store and posts it as a system note; a plain confirmation line comes back.
  const image = (path: string, note?: string) =>
    hostCall("image", note === undefined ? [path] : [path, note]);
  // Ship: commit (+push) the session's work into the origin repo; options and the
  // result object both travel as JSON, like mcp().
  const ship = async (opts?: unknown) =>
    JSON.parse(await hostCall("ship", [JSON.stringify(opts ?? {})]));
  // PR: export the session's work into a branch and open a GitHub PR; options and
  // the PrResult both travel as JSON, like ship().
  const pr = async (opts?: unknown) =>
    JSON.parse(await hostCall("pr", [JSON.stringify(opts ?? {})]));
  // Recurring runs: one host function fanned out as a method object, like lsp.*;
  // JSON round-trip both ways.
  const scheduleCall = async (verb: string, args?: unknown) =>
    JSON.parse(await hostCall("schedule", [verb, JSON.stringify(args ?? null)]));
  const schedule = Object.fromEntries(
    ["list", "add", "enable", "disable", "remove"].map(
      (verb) => [verb, (args?: unknown) => scheduleCall(verb, args)],
    ),
  );
  // Durable notes for this conversation: one host function fanned out as a method
  // object, like schedule.*; get() returns null for an unset key.
  const stateCall = async (verb: string, args?: unknown) =>
    JSON.parse(await hostCall("state", [verb, JSON.stringify(args ?? null)]));
  const state = Object.fromEntries(
    ["get", "set", "list", "delete"].map(
      (verb) => [verb, (args?: unknown) => stateCall(verb, args)],
    ),
  );
  // Workflows: one host function fanned out as a method object, like schedule.*.
  const workflowCall = async (verb: string, args?: unknown) =>
    JSON.parse(await hostCall("workflow", [verb, JSON.stringify(args ?? null)]));
  const workflow = Object.fromEntries(
    ["start", "rerun", "stop", "pause", "resume", "status", "list"].map(
      (verb) => [verb, (args?: unknown) => workflowCall(verb, args)],
    ),
  );

  // deno-lint-ignore no-explicit-any
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor as any;
  const program = new AsyncFunction(
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
    "ship",
    "pr",
    "schedule",
    "state",
    "workflow",
    "console",
    code,
  );
  await program(
    bash,
    sh,
    extract,
    fetch,
    bashBg,
    bashOutput,
    bashWait,
    bashKill,
    read,
    write,
    edit,
    agent,
    spawn,
    join,
    adopt,
    ask,
    mcp,
    mcpStatus,
    lsp,
    artifact,
    recall,
    image,
    ship,
    pr,
    schedule,
    state,
    workflow,
    sandboxConsole,
  );
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
