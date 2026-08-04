/**
 * The program side of the program worker. This module IS the worker: a Bun worker
 * inherits the server process's capabilities, so the program it executes has
 * everything the server itself has — filesystem, network, env, subprocesses,
 * `npm:` imports. The host functions bridged in over `postMessage` are convenience
 * and session integration, not a boundary, and a program may ignore all of them and
 * call `Bun`/`node:*` directly (spec §2.2).
 *
 * THE INVARIANT THIS HOLDS: **because the isolate is not sealed, everything the
 * program can start must be stoppable from here.** Two things it can start would
 * otherwise outlive the turn or take the server down:
 *
 *   1. **The exit trap.** `process.exit()` terminates the worker silently — the
 *      turn then hangs until its wall timeout with no error to report. With
 *      inherited capabilities the stakes are worse: an uncaught exit here can take
 *      the whole bough server with it (plan §6.2). Weak models emit
 *      `process.exit(1)` as an "assertion failed" idiom, so it is replaced with a
 *      throw the round can catch and report.
 *   2. **Child-process tracking.** Every process the program spawns natively is a
 *      child of the SERVER process, and `worker.terminate()` does not touch it. So
 *      `Bun.spawn` is wrapped to record what it spawns, and the abort handshake
 *      sweeps the set with SIGTERM *before* acking — children first, then the
 *      worker (plan §6.3). `node:child_process` routes through `Bun.spawn`, so it
 *      is covered by the same wrapper. Only the async path is tracked:
 *      `Bun.spawnSync()` blocks this worker's event loop, so an abort message could
 *      not be handled during one anyway. `Bun.$` does NOT route through `Bun.spawn`
 *      — the same kind of second entry point `Deno.Command`'s `output()` was before
 *      the port — and it exposes no pid and no kill handle, so it cannot be tracked.
 *      It is therefore REMOVED from the program's reach rather than left as a silent
 *      hole: an untracked shell survives the sweep, and the interrupt then tells the
 *      user "the program's children are killed" while a process keeps running.
 *
 * A third interception, for a different reason: SHELL-SHAPED process creation is
 * shut across `Bun.spawn`/`Bun.spawnSync` and `node:child_process`, because a shell
 * run that way leaves no row in the command memory that `bash(cmd, tags)` feeds.
 * That is a memory boundary, not a security one — see the block itself for why the
 * rule stops at shells and leaves direct binary spawning open.
 *
 * The program's parameters come from `protocol.ts` — the same list the host's
 * pre-flight check parses with, so the two cannot disagree about which names are
 * taken (see that module's header). This file declares no name of its own; the
 * `satisfies Record<HostFnName, …>` on the binding table is what makes a drift a
 * typecheck failure rather than a runtime hole.
 *
 * `console.*` lines are BOTH streamed (`{type:"log"}` — the UI renders them live)
 * AND batched into `logs`, so the model still receives the full output in its tool
 * result (spec §5). Display-only streaming: context contents are unchanged.
 *
 * Ported from `src/harness/vm_worker.ts`. Deltas are marked `NOTE:`.
 */

import { createRequire } from "node:module";
import {
  type FromProgramWorker,
  HOST_FN_VERBS,
  type HostFnName,
  PROGRAM_PARAMS,
  type ProgramParam,
  type ToProgramWorker,
} from "./protocol.ts";

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

const pending = new Map<number, { resolve: (v: string) => void; reject: (e: Error) => void }>();
let seq = 0;
const logs: string[] = [];

const send = (msg: FromProgramWorker) => self.postMessage(msg);

function hostCall(fn: HostFnName, args: unknown[]): Promise<string> {
  const id = ++seq;
  const p = new Promise<string>((resolve, reject) => pending.set(id, { resolve, reject }));
  send({ type: "host", id, fn, args });
  return p;
}

/** Every structured host result crosses the wire as JSON — the protocol is string-only. */
const jsonCall = async (fn: HostFnName, args: unknown[]): Promise<unknown> =>
  JSON.parse(await hostCall(fn, args));

/**
 * A verb-dispatched host function, rebuilt worker-side as the method object the
 * program actually calls (`state.get(...)` → `state("get", argsJson)`). The verb
 * lists live in `protocol.ts` so the host dispatcher and this cannot drift.
 */
function methodObject(fn: "state" | "schedule" | "workflow") {
  const verbs: readonly string[] = HOST_FN_VERBS[fn];
  return Object.fromEntries(
    verbs.map((
      verb,
    ) => [verb, (args?: unknown) => jsonCall(fn, [verb, JSON.stringify(args ?? null)])]),
  );
}

// ---------------------------------------------------------------------------
// console — streamed and batched
// ---------------------------------------------------------------------------

function show(v: unknown): string {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// A console.* call emits its line immediately (live progress in the UI) AND keeps
// it in the batch (the model-facing tool result ships the joined logs).
const print = (...args: unknown[]) => {
  const line = args.map(show).join(" ");
  logs.push(line);
  send({ type: "log", line });
};
const programConsole = { log: print, error: print, warn: print, info: print, debug: print };

// ---------------------------------------------------------------------------
// The exit trap
// ---------------------------------------------------------------------------

// NOTE: the ported message called this a "sandbox". It is not one, and saying so
// would be exactly the implied safety spec §2.2 forbids — so the message now says
// what is true: a program ends by returning.
const exitTrap = (code?: unknown): never => {
  throw new Error(
    `exit(${code ?? 0}) is not available to a program — a program ends by returning, ` +
      `and signals failure by throwing an Error. Calling exit() would terminate the ` +
      `worker mid-turn with no result to report.`,
  );
};
try {
  const g = globalThis as { process?: { exit?: unknown } };
  if (g.process) g.process.exit = exitTrap;
} catch { /* frozen globals — nothing to guard */ }

// ---------------------------------------------------------------------------
// Child-process tracking
// ---------------------------------------------------------------------------

const children = new Set<Bun.Subprocess>();

function trackChild(child: Bun.Subprocess): Bun.Subprocess {
  children.add(child);
  // Reap on exit either way — a rejected status is still an exit.
  child.exited.catch(() => {}).finally(() => children.delete(child));
  return child;
}

function killChildren(): void {
  for (const child of children) {
    try {
      child.kill("SIGTERM");
    } catch { /* already exited between the sweep and the signal */ }
  }
  children.clear();
}

// ---------------------------------------------------------------------------
// The shell doors
// ---------------------------------------------------------------------------

/**
 * A shell run from inside a program is invisible to the COMMAND MEMORY: `bash()`
 * records its command under the tags the model chose, and `bough tags` /
 * `history.sql()` are built on nothing else. `execSync("rg …")` produces the same
 * output and leaves no row, so the memory silently under-reports what the agent
 * actually does — and a model that can reach the untagged door takes it, because
 * tagging is work and `execSync` is not (observed: a session that ran 54 commands
 * through `node:child_process` and zero through `bash`).
 *
 * So the shell-shaped doors are shut, with an error naming the one that is open.
 * The rule is narrow on purpose — SHELL-SHAPED process creation only. Spawning a
 * binary directly (`Bun.spawn(["bun", "x.ts"])`, a piped long-lived server, stdin
 * streaming) is real work the host functions do not cover, and it stays open, as do
 * fs, network and `npm:` imports. This is a memory boundary, not a security one:
 * §2.2 still holds, and a program that truly wants a shell can still write one out
 * and exec it. It is a door that costs more than tagging, which is the whole point.
 */
const SHELLS = new Set([
  "sh",
  "bash",
  "zsh",
  "dash",
  "ksh",
  "fish",
  "csh",
  "tcsh",
  "pwsh",
  "powershell",
  "cmd",
  "cmd.exe",
]);

function shellDoorShut(what: string): Error {
  return new Error(
    `${what} is not available inside a program — a command run that way is absent from ` +
      `your command history, so no future session can recall it. Use await bash(cmd, tags) ` +
      `for one command, sh(a, b, …) to run several at once, or bashBg(name, cmd) for work ` +
      `that should outlive the round. Spawning a binary directly is still fine.`,
  );
}

/** `shell: true`, or an argv whose program IS a shell. Either way it is a shell. */
function isShellSpawn(cmd: unknown, ...maybeOpts: unknown[]): boolean {
  for (const o of maybeOpts) {
    if (o && typeof o === "object" && (o as { shell?: unknown }).shell) return true;
  }
  const argv0 = Array.isArray(cmd) ? cmd[0] : cmd;
  if (typeof argv0 !== "string") return false;
  return SHELLS.has(argv0.split("/").pop() ?? "");
}

/** `Bun.spawn` is overloaded — `(cmd, opts)` and `({cmd, …})` both reach here. */
function isShellBunSpawn(args: unknown[]): boolean {
  const [first, second] = args;
  if (Array.isArray(first)) return isShellSpawn(first, second);
  if (first && typeof first === "object") return isShellSpawn((first as { cmd?: unknown }).cmd, first);
  return false;
}

try {
  // `Bun.spawn` is a function, not a class, so this is a forwarding wrapper rather
  // than a subclass — and a plain assignment, because the property is writable but
  // NOT configurable (defineProperty would throw).
  //
  // The wrapper is deliberately signature-agnostic: `Bun.spawn` is overloaded
  // (`spawn(cmd, opts)` and `spawn({cmd, …})`), and re-declaring either form here
  // would be a second copy of a contract that is not ours to restate. Arguments go
  // through untouched, so every default — including which streams are piped — is
  // still Bun's, and the wrapper is invisible to the caller.
  const realSpawn = Bun.spawn;
  const spawnAny = realSpawn as unknown as (...args: unknown[]) => Bun.Subprocess;
  Bun.spawn = ((...args: unknown[]) => {
    if (isShellBunSpawn(args)) throw shellDoorShut("Bun.spawn of a shell");
    return trackChild(spawnAny(...args));
  }) as typeof Bun.spawn;

  const realSpawnSync = Bun.spawnSync;
  const spawnSyncAny = realSpawnSync as unknown as (...args: unknown[]) => unknown;
  Bun.spawnSync = ((...args: unknown[]) => {
    if (isShellBunSpawn(args)) throw shellDoorShut("Bun.spawnSync of a shell");
    return spawnSyncAny(...args);
  }) as typeof Bun.spawnSync;
} catch { /* namespace locked down — natively spawned children stay untracked */ }

// `node:child_process` routes through `Bun.spawn`, so the wrapper above already
// SEES it — but it sees an argv, and blocking there would report the wrong door
// ("Bun.spawn" to a program that wrote `execSync`). Patching the module's own
// exports names what the program actually called. Mutating the CJS export object
// is enough for every spelling: `import("node:child_process")`, a destructured
// import, and `require("child_process")` all resolve to this same object.
try {
  const cp = createRequire(import.meta.url)("node:child_process") as Record<string, unknown>;
  // `exec`/`execSync` take a command LINE — they are a shell by definition, with no
  // shape to check.
  for (const name of ["exec", "execSync"]) {
    cp[name] = () => {
      throw shellDoorShut(`child_process.${name}`);
    };
  }
  // The rest take a program and an argv, so they are only a shell when they say so.
  for (const name of ["spawn", "spawnSync", "execFile", "execFileSync"]) {
    const real = cp[name] as (...args: unknown[]) => unknown;
    if (typeof real !== "function") continue;
    cp[name] = (...args: unknown[]) => {
      if (isShellSpawn(args[0], args[1], args[2])) throw shellDoorShut(`child_process.${name}`);
      return real(...args);
    };
  }
} catch { /* the module could not be patched — the untagged door stays open */ }

// `Bun.$` has no kill handle and does not route through `Bun.spawn` (verified: a
// `Bun.$` shell produces zero calls through the wrapper above), so a shell started
// with it survives the abort sweep — the interrupt then reports "the program's
// children are killed and the partial result is kept" while a process keeps running,
// reparented onto the server. A hole that reports itself closed is worse than a
// missing feature, so the door is shut and the error says which door to use instead.
// Programs get bash()/sh()/bashBg(): tracked, killable, buffered, and listed in the
// jobs tab.
try {
  Bun.$ = (() => {
    throw new Error(
      "Bun.$ is not available inside a program — a shell started with it cannot be " +
        "interrupted. Use bash(cmd) for one command, sh(a, b, …) to run several at " +
        "once, or bashBg(name, cmd) for work that should outlive the round.",
    );
  }) as unknown as typeof Bun.$;
} catch { /* frozen namespace — the hole documented in the header stays open */ }

// ---------------------------------------------------------------------------
// The program's scope
// ---------------------------------------------------------------------------

/**
 * One binding per name in `HOST_FN_NAMES`. The `satisfies` clause is load-bearing:
 * a name added to the protocol without a binding here, or a binding here that the
 * protocol does not declare, fails `bun run check`.
 *
 * Where a signature takes an object, it is serialized on the way out and the result
 * is parsed on the way back, so the program deals in real objects while the wire
 * stays string-only. `view`/`patch` are the exception — their text IS the payload.
 */
const bindings = {
  // Tags always cross the wire, even absent, so the host can enforce the required
  // param with its corrective ProgramError instead of an arity surprise.
  bash: (cmd: string, tags?: string) => hostCall("bash", [cmd, tags ?? ""]),
  // Concurrent shells: commands ride out as a JSON array, `[{code, out}, …]` comes
  // back as JSON. A non-zero code is DATA here, never a throw.
  //
  // Two call shapes. Variadic `sh("a", "b")` runs untagged. Array-first
  // `sh([{cmd, tag}, …])` tags each leg for the command history — accepted
  // because array-first is the shape models actually reach for when they want
  // tagged legs (observed live: a model passed an array, got "non-string
  // element", and its retry silently dropped the tags). Both shapes travel as
  // one JSON array; the host validates elements.
  sh: (...args: unknown[]) =>
    jsonCall("sh", [JSON.stringify(Array.isArray(args[0]) ? args[0] : args)]),
  // Background shells: the handle comes back as JSON ({id, pid}); output/kill are
  // plain text.
  bashBg: (name: string, cmd: string) => jsonCall("bashBg", [name, cmd]),
  bashOutput: (id: string) => hostCall("bashOutput", [id]),
  bashWait: (id: string) => hostCall("bashWait", [id]),
  bashKill: (id: string) => hostCall("bashKill", [id]),
  // Hash-anchored editing: view() returns numbered lines under a content tag and
  // patch() applies ops anchored to it. Plain strings both ways.
  view: (path: string) => hostCall("view", [path]),
  patch: (input: string) => hostCall("patch", [input]),
  write: (path: string, content: string) => hostCall("write", [path, content]),
  // Delegation. A session that may not delegate has no bridged fn, and the call
  // rejects catchably — which is correct, because the prompt omits the section too.
  agent: (task: string, opts?: unknown) => jsonCall("agent", [task, JSON.stringify(opts ?? {})]),
  spawn: (task: string, opts?: unknown) => jsonCall("spawn", [task, JSON.stringify(opts ?? {})]),
  join: (sessionId: string) => jsonCall("join", [sessionId]),
  adopt: (sessionId: string) => hostCall("adopt", [sessionId]),
  workflow: methodObject("workflow"),
  // Ask the human: options ride out as JSON, the answer comes back as a plain
  // string. Rejects catchably on decline or interrupt.
  ask: (question: string, opts?: unknown) =>
    hostCall("ask", [question, JSON.stringify(opts ?? {})]),
  state: methodObject("state"),
  schedule: methodObject("schedule"),
  // A non-string content (an object) is stringified so programs can pass it directly.
  artifact: (name: string, content: unknown) =>
    jsonCall("artifact", [name, typeof content === "string" ? content : JSON.stringify(content)]),
} satisfies Record<HostFnName, unknown>;

/**
 * Everything the program's scope holds: the bridged names, `console`, `require`.
 *
 * The `require` is a REAL one, resolving from this module's location, not a stub
 * that throws a friendlier message — the program already has the capabilities it
 * would reach for (spec §2.2), so the only thing missing was the CommonJS spelling.
 */
const scope: Record<ProgramParam, unknown> = {
  ...bindings,
  console: programConsole,
  require: createRequire(import.meta.url),
};

async function run(code: string): Promise<void> {
  // deno-lint-ignore no-explicit-any
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor as any;
  // Built from PROGRAM_PARAMS, in order, so the parameter list the worker binds is
  // the same array the host pre-flights against. Nothing is spelled out twice.
  const program = new AsyncFunction(...PROGRAM_PARAMS, code);
  await program(...PROGRAM_PARAMS.map((name) => scope[name]));
}

self.onmessage = (e: MessageEvent) => {
  const msg = e.data as ToProgramWorker;
  // Stop requested (interrupt or timeout): kill what the program spawned, THEN tell
  // the host it is safe to terminate us. Host-side work (a foreground bash) is
  // already dying on the turn's own signal.
  if (msg.type === "abort") {
    killChildren();
    send({ type: "aborted" });
    return;
  }
  if (msg.type === "host_result") {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    if (!p) return;
    if (msg.ok) p.resolve(msg.value);
    else p.reject(new Error(msg.value));
    return;
  }
  run(msg.code)
    .then(() => send({ type: "done", logs }))
    .catch((err) => send({ type: "error", message: String((err as Error)?.stack ?? err), logs }));
};
