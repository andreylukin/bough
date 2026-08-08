// The program side of the program worker — adapted from src/harness/vm_worker.ts
// for the Rust port: postMessage → stdout NDJSON, onmessage → readline on stdin.
// Runs under Bun or Node >= 20 as CommonJS; harness/vm.rs ships this file via
// include_str! and materializes it to a cache dir at first use. Do not edit the
// cached copy — edit crates/bough-core/src/harness/js/vm_worker.js.
//
// The sidecar inherits the server process's capabilities, so the program it
// executes has everything the server itself has — filesystem, network, env,
// subprocesses, `npm:`/`node:*` imports. The host functions bridged in over
// stdin are convenience and session integration, not a boundary (spec §2.2).
//
// THE INVARIANT THIS HOLDS: **because the isolate is not sealed, everything the
// program can start must be stoppable from here.**
//
//   1. The exit trap: `process.exit()` would end the sidecar silently with no
//      result to report — it is replaced with a throw the round can catch.
//   2. Child-process tracking: spawned processes are recorded, and the abort
//      handshake sweeps the set with SIGTERM *before* acking — children first,
//      then the worker.
//
// A third interception, for a different reason: SHELL-SHAPED process creation
// is shut, because a shell run that way leaves no row in the command memory
// that `bash(cmd, tags)` feeds. That is a memory boundary, not a security one.
"use strict";

const readline = require("node:readline");
const { Console } = require("node:console");
const { createRequire } = require("node:module");
const nodePath = require("node:path");

// Saved before the exit trap lands: the worker itself still needs a real exit
// when the host closes stdin.
const realExit = process.exit.bind(process);

// stdout IS the protocol channel. The worker's own console — and any stray
// global console use by code the program imports — goes to stderr so it can
// never corrupt the NDJSON stream. The program's `console` is the bound
// parameter below, which streams AND batches.
try {
  globalThis.console = new Console(process.stderr, process.stderr);
} catch { /* frozen globals */ }

const IS_BUN = typeof Bun !== "undefined";

// ---------------------------------------------------------------------------
// Mirrors of protocol.rs. The Rust probe test runs a real program printing
// `typeof` of every name — that test is what keeps these lists and the Rust
// lists from drifting (it replaces the TS shared-import invariant).
// ---------------------------------------------------------------------------

const HOST_FN_NAMES = [
  // shell
  "bash",
  "sh",
  "bashBg",
  "bashOutput",
  "bashWait",
  "bashKill",
  // files — the one editing idiom
  "view",
  "patch",
  "write",
  // delegation
  "agent",
  "spawn",
  "join",
  "adopt",
  // orchestration
  "workflow",
  // session verbs
  "ask",
  "state",
  "schedule",
  "artifact",
];

const PROGRAM_PARAMS = [...HOST_FN_NAMES, "console", "require"];

const HOST_FN_VERBS = {
  state: ["get", "set", "list", "delete"],
  schedule: ["list", "add", "enable", "disable", "remove"],
  workflow: ["start", "rerun", "stop", "pause", "resume", "status", "list"],
};

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

const send = (msg) => {
  process.stdout.write(JSON.stringify(msg) + "\n");
};

const pending = new Map();
let seq = 0;
const logs = [];

function hostCall(fn, args) {
  const id = ++seq;
  const p = new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
  send({ type: "host", id, fn, args });
  return p;
}

// Every structured host result crosses the wire as JSON — the protocol is
// string-only.
const jsonCall = async (fn, args) => JSON.parse(await hostCall(fn, args));

// A verb-dispatched host function, rebuilt worker-side as the method object the
// program actually calls (`state.get(...)` → `state("get", argsJson)`).
function methodObject(fn) {
  const verbs = HOST_FN_VERBS[fn];
  return Object.fromEntries(
    verbs.map((verb) => [verb, (args) => jsonCall(fn, [verb, JSON.stringify(args ?? null)])]),
  );
}

// ---------------------------------------------------------------------------
// console — streamed and batched
// ---------------------------------------------------------------------------

function show(v) {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

// A console.* call emits its line immediately (live progress in the UI) AND
// keeps it in the batch (the model-facing tool result ships the joined logs).
const print = (...args) => {
  const line = args.map(show).join(" ");
  logs.push(line);
  send({ type: "log", line });
};
const programConsole = { log: print, error: print, warn: print, info: print, debug: print };

// ---------------------------------------------------------------------------
// The exit trap
// ---------------------------------------------------------------------------

const exitTrap = (code) => {
  throw new Error(
    `exit(${code ?? 0}) is not available to a program — a program ends by returning, ` +
      `and signals failure by throwing an Error. Calling exit() would terminate the ` +
      `worker mid-turn with no result to report.`,
  );
};
try {
  if (process) process.exit = exitTrap;
} catch { /* frozen globals — nothing to guard */ }

// ---------------------------------------------------------------------------
// Child-process tracking
// ---------------------------------------------------------------------------

const children = new Set();

// Tracks both shapes: a Bun.Subprocess (`.exited` promise) and a Node
// ChildProcess (`exit`/`error` events). Reaped on exit either way.
function trackChild(child) {
  if (!child || typeof child.kill !== "function") return child;
  children.add(child);
  const reap = () => children.delete(child);
  if (child.exited && typeof child.exited.then === "function") {
    child.exited.catch(() => {}).finally(reap);
  } else if (typeof child.once === "function") {
    child.once("exit", reap);
    child.once("error", reap);
  }
  return child;
}

function killChildren() {
  for (const child of children) {
    try {
      child.kill("SIGTERM");
    } catch { /* already exited between the sweep and the signal */ }
  }
  children.clear();
}

// ---------------------------------------------------------------------------
// The shell doors — a memory boundary, not a security one. A shell run from
// inside a program is invisible to the COMMAND MEMORY: `bash()` records its
// command under the tags the model chose; `execSync("rg …")` produces the same
// output and leaves no row. The rule is narrow on purpose: SHELL-SHAPED
// process creation only. Spawning a binary directly stays open, as do fs,
// network and `npm:` imports (spec §2.2 still holds).
// ---------------------------------------------------------------------------

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

function shellDoorShut(what) {
  return new Error(
    `${what} is not available inside a program — a command run that way is absent from ` +
      `your command history, so no future session can recall it. Use await bash(cmd, tags) ` +
      `for one command, sh(a, b, …) to run several at once, or bashBg(name, cmd) for work ` +
      `that should outlive the round. Spawning a binary directly is still fine.`,
  );
}

// `shell: true`, or an argv whose program IS a shell. Either way it is a shell.
function isShellSpawn(cmd, ...maybeOpts) {
  for (const o of maybeOpts) {
    if (o && typeof o === "object" && o.shell) return true;
  }
  const argv0 = Array.isArray(cmd) ? cmd[0] : cmd;
  if (typeof argv0 !== "string") return false;
  return SHELLS.has(argv0.split("/").pop() ?? "");
}

// `Bun.spawn` is overloaded — `(cmd, opts)` and `({cmd, …})` both reach here.
function isShellBunSpawn(args) {
  const [first, second] = args;
  if (Array.isArray(first)) return isShellSpawn(first, second);
  if (first && typeof first === "object") return isShellSpawn(first.cmd, first);
  return false;
}

if (IS_BUN) {
  try {
    // A forwarding wrapper, signature-agnostic, arguments untouched — a plain
    // assignment, because the property is writable but NOT configurable.
    const realSpawn = Bun.spawn;
    Bun.spawn = (...args) => {
      if (isShellBunSpawn(args)) throw shellDoorShut("Bun.spawn of a shell");
      return trackChild(realSpawn(...args));
    };

    // Only the async path is tracked: `Bun.spawnSync()` blocks this worker's
    // event loop, so an abort message could not be handled during one anyway.
    const realSpawnSync = Bun.spawnSync;
    Bun.spawnSync = (...args) => {
      if (isShellBunSpawn(args)) throw shellDoorShut("Bun.spawnSync of a shell");
      return realSpawnSync(...args);
    };
  } catch { /* namespace locked down — natively spawned children stay untracked */ }

  // `Bun.$` has no kill handle and does not route through `Bun.spawn`, so a
  // shell started with it would survive the abort sweep — the interrupt then
  // reports "the program's children are killed" while a process keeps running.
  // A hole that reports itself closed is worse than a missing feature.
  try {
    Bun.$ = () => {
      throw new Error(
        "Bun.$ is not available inside a program — a shell started with it cannot be " +
          "interrupted. Use bash(cmd) for one command, sh(a, b, …) to run several at " +
          "once, or bashBg(name, cmd) for work that should outlive the round.",
      );
    };
  } catch { /* frozen namespace — the hole documented in the header stays open */ }
}

// `node:child_process` is patched via its CJS export object, which covers every
// spelling: `import("node:child_process")`, a destructured import, and
// `require("child_process")` all resolve to this same object — and the error
// then names the door the program actually used, not "Bun.spawn". Under Node
// this patch is ALSO where child tracking happens (there is no Bun.spawn to
// route through), so the async creators wrap their result in trackChild.
try {
  const cp = require("node:child_process");
  // `exec`/`execSync` take a command LINE — they are a shell by definition,
  // with no shape to check.
  for (const name of ["exec", "execSync"]) {
    cp[name] = () => {
      throw shellDoorShut(`child_process.${name}`);
    };
  }
  // The rest take a program and an argv, so they are only a shell when they
  // say so. `spawn`/`execFile` return a killable child — track it.
  for (const name of ["spawn", "execFile"]) {
    const real = cp[name];
    if (typeof real !== "function") continue;
    cp[name] = (...args) => {
      if (isShellSpawn(args[0], args[1], args[2])) throw shellDoorShut(`child_process.${name}`);
      return trackChild(real(...args));
    };
  }
  for (const name of ["spawnSync", "execFileSync"]) {
    const real = cp[name];
    if (typeof real !== "function") continue;
    cp[name] = (...args) => {
      if (isShellSpawn(args[0], args[1], args[2])) throw shellDoorShut(`child_process.${name}`);
      return real(...args);
    };
  }
} catch { /* the module could not be patched — the untagged door stays open */ }

// ---------------------------------------------------------------------------
// The program's scope
// ---------------------------------------------------------------------------

// One binding per name in HOST_FN_NAMES, verbatim from vm_worker.ts. Where a
// signature takes an object, it is serialized on the way out and the result is
// parsed on the way back, so the program deals in real objects while the wire
// stays string-only. `view`/`patch` are the exception — their text IS the
// payload.
const bindings = {
  // Tags always cross the wire, even absent, so the host can enforce the
  // required param with its corrective ProgramError instead of an arity
  // surprise.
  bash: (cmd, tags) => hostCall("bash", [cmd, tags ?? ""]),
  // Two call shapes: variadic sh("a", "b") runs untagged; array-first
  // sh([{cmd, tag}, …]) tags each leg. Both travel as one JSON array. A
  // non-zero code is DATA here, never a throw.
  sh: (...args) => jsonCall("sh", [JSON.stringify(Array.isArray(args[0]) ? args[0] : args)]),
  bashBg: (name, cmd) => jsonCall("bashBg", [name, cmd]),
  bashOutput: (id) => hostCall("bashOutput", [id]),
  bashWait: (id) => hostCall("bashWait", [id]),
  bashKill: (id) => hostCall("bashKill", [id]),
  view: (path) => hostCall("view", [path]),
  patch: (input) => hostCall("patch", [input]),
  write: (path, content) => hostCall("write", [path, content]),
  agent: (task, opts) => jsonCall("agent", [task, JSON.stringify(opts ?? {})]),
  spawn: (task, opts) => jsonCall("spawn", [task, JSON.stringify(opts ?? {})]),
  join: (sessionId) => jsonCall("join", [sessionId]),
  adopt: (sessionId) => hostCall("adopt", [sessionId]),
  workflow: methodObject("workflow"),
  ask: (question, opts) => hostCall("ask", [question, JSON.stringify(opts ?? {})]),
  state: methodObject("state"),
  schedule: methodObject("schedule"),
  artifact: (name, content) =>
    jsonCall("artifact", [name, typeof content === "string" ? content : JSON.stringify(content)]),
};

// The `require` is a REAL one — the program already has the capabilities it
// would reach for (spec §2.2); the only thing missing was the CommonJS
// spelling. It resolves from the session's working directory (this file lives
// in a cache dir with no node_modules of its own), so `node:*` builtins work
// everywhere and project packages resolve from the project.
const programRequire = createRequire(nodePath.join(process.cwd(), "__program__.cjs"));

const scope = {
  ...bindings,
  console: programConsole,
  require: programRequire,
};

// ---------------------------------------------------------------------------
// Extensions — user JavaScript bound into the same scope
// ---------------------------------------------------------------------------
//
// An extension function is NOT bridged: it is required here and called
// in-process, so it never crosses the wire and the host's closed HOST_FN_NAMES
// list is untouched. What it gets in exchange for that is the host functions
// themselves — `bindings` is passed to the module's factory export, so an
// extension composes bash()/view()/patch() instead of reimplementing them.
//
// Names bound here are appended to the program's parameter list, which is why
// the host sends `extensions` BEFORE `check`: the pre-flight parse must know
// about them or a program using one would fail to compile.

const extensionNames = [];

const IDENT = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

// The declared parameter list, read off the engine's own toString. Nested
// parens in a default value (`(a = f(1))`) would break the cheap scan, so a
// depth counter walks it; anything unreadable degrades to "()", never throws.
function signatureOf(fn) {
  let src;
  try {
    src = Function.prototype.toString.call(fn);
  } catch {
    return "()";
  }
  const open = src.indexOf("(");
  if (open === -1) {
    // A bare arrow with one unparenthesized param: `x => …`.
    const arrow = src.indexOf("=>");
    const head = arrow === -1 ? "" : src.slice(0, arrow).trim();
    return IDENT.test(head) ? `(${head})` : "()";
  }
  let depth = 0;
  for (let i = open; i < src.length; i++) {
    if (src[i] === "(") depth++;
    else if (src[i] === ")") {
      depth--;
      if (depth === 0) return src.slice(open, i + 1).replace(/\s+/g, " ");
    }
  }
  return "()";
}

// Both module shapes: `module.exports = {…}` / named ESM exports, and a
// default export object. A default-exported FUNCTION is treated as a factory
// (called with the host bindings) so an extension can close over bash() —
// that is the only way it reaches the session at all.
function exportsOf(mod) {
  if (!mod || (typeof mod !== "object" && typeof mod !== "function")) return {};
  const dflt = mod.default;
  if (typeof dflt === "function") {
    const produced = dflt(bindings);
    return produced && typeof produced === "object" ? produced : {};
  }
  if (dflt && typeof dflt === "object") return { ...dflt, ...mod };
  if (typeof mod === "function") {
    const produced = mod(bindings);
    return produced && typeof produced === "object" ? produced : {};
  }
  return mod;
}

function loadExtensions(files) {
  const fns = [];
  const errors = [];
  for (const file of files ?? []) {
    let exported;
    try {
      exported = exportsOf(programRequire(file));
    } catch (err) {
      errors.push(`${file}: ${String((err && err.message) ?? err)}`);
      continue;
    }
    for (const name of Object.keys(exported)) {
      let value;
      try {
        value = exported[name];
      } catch {
        continue; // a throwing getter is not an export we can bind
      }
      if (typeof value !== "function") continue;
      if (name === "default") continue;
      if (!IDENT.test(name)) {
        errors.push(`${file}: export "${name}" is not a usable identifier — skipped`);
        continue;
      }
      // The eighteen (and console/require) win. Shadowing bash() from an
      // extension would silently redefine the memory boundary.
      if (PROGRAM_PARAMS.includes(name)) {
        errors.push(`${file}: "${name}" is already bound in every program — skipped`);
        continue;
      }
      // Later file wins, matching the host's binding order (global, then
      // project). Replace the binding AND the reported entry, so the prompt
      // never documents a function that is not the one bound.
      const already = extensionNames.indexOf(name);
      if (already !== -1) fns.splice(fns.findIndex((f) => f.name === name), 1);
      else extensionNames.push(name);
      scope[name] = value;
      fns.push({
        name,
        signature: signatureOf(value),
        doc: typeof value.doc === "string" ? value.doc : null,
        file,
      });
    }
  }
  return { fns, errors };
}

// The full parameter list: the fixed ones, then whatever extensions bound.
// Read through a function because extensions land after this module is
// evaluated — a captured array would be the pre-extension one.
const programParams = () => [...PROGRAM_PARAMS, ...extensionNames];

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

async function run(code) {
  // Built from the same list the host pre-flighted against, in the same order.
  const params = programParams();
  const program = new AsyncFunction(...params, code);
  await program(...params.map((name) => scope[name]));
}

// ---------------------------------------------------------------------------
// stdin — the message loop
// ---------------------------------------------------------------------------

const rl = readline.createInterface({ input: process.stdin, terminal: false });

rl.on("line", (raw) => {
  let msg;
  try {
    msg = JSON.parse(raw);
  } catch {
    return; // not protocol — dropped
  }
  // Stop requested (interrupt or timeout): kill what the program spawned,
  // THEN tell the host it is safe to terminate us.
  if (msg.type === "abort") {
    killChildren();
    send({ type: "aborted" });
    return;
  }
  if (msg.type === "host_result") {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    if (!p) return; // unknown pending id — dropped silently
    if (msg.ok) p.resolve(msg.value);
    else p.reject(new Error(msg.value));
    return;
  }
  // Bind user JavaScript into the program scope. Always answered, even when
  // every file failed — the host waits on this before sending `check`.
  if (msg.type === "extensions") {
    let loaded;
    try {
      loaded = loadExtensions(msg.files);
    } catch (err) {
      loaded = { fns: [], errors: [String((err && err.message) ?? err)] };
    }
    send({ type: "extensions_result", fns: loaded.fns, errors: loaded.errors });
    return;
  }
  // Pre-flight parse, delegated here for engine parity: constructing the
  // AsyncFunction parses, it does not execute, and the code never touches this
  // scope. The host shapes the model-facing message from the raw engine words.
  if (msg.type === "check") {
    try {
      new AsyncFunction(...programParams(), msg.code);
      send({ type: "check_result" });
    } catch (err) {
      send({
        type: "check_result",
        name: (err && err.name) || "Error",
        message: String((err && err.message) ?? err),
      });
    }
    return;
  }
  if (msg.type === "run") {
    run(msg.code)
      .then(() => send({ type: "done", logs }))
      .catch((err) => send({ type: "error", message: String((err && err.stack) ?? err), logs }));
  }
});

// The host closed stdin: the turn is over either way. Use the saved real exit
// — `process.exit` is trapped above.
rl.on("close", () => realExit(0));
