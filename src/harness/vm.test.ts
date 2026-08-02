/**
 * The program worker bridge, tested through a REAL worker with trivial programs
 * (plan §7: "Workers | Real workers, trivial programs. Assert on the bridge
 * protocol."). Nothing here mocks `postMessage` — the things that can go wrong are
 * ordering and lifecycle, and a fake bridge would prove neither.
 *
 * Four of these tests are invariant tests, not feature tests. They fail loudly and
 * they are the reason this file exists:
 *
 *   - **the two host-name lists match** — the pre-flight check and the worker must
 *     agree about which names are taken, or a program passes validation and then
 *     fails to compile inside the worker with the model left guessing (plan T3.1).
 *   - **a shadowed host name fails pre-flight** — the same invariant from the
 *     program's side.
 *   - **`process.exit()` is catchable** — uncaught, it terminates the worker
 *     silently and, since a Bun worker inherits the server's capabilities, can take
 *     the server with it (plan §6.2).
 *   - **an aborted program leaves no orphan** — children are killed BEFORE the
 *     worker is terminated; reverse order orphans processes (plan §6.3).
 *
 * Hermetic and offline: no network, no `~/.bough`, no API keys. The one test that
 * spawns a process uses `sh` in a temp dir and cleans up after itself.
 *
 * Assertions come from `node:assert` rather than a package: it is built into the
 * runtime and needs no fetch, and this environment's egress policy denies the
 * registries. (Same constraint `bus.test.ts`, `paths.test.ts` and
 * `hostfn/patch.test.ts` document.)
 */

import { test } from "bun:test";
import { ok, strictEqual } from "node:assert";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { HostFns } from "../types.ts";
import { HOST_FN_NAMES, PROGRAM_PARAMS } from "./protocol.ts";
import { checkProgramSyntax, runProgram, unterminatedString } from "./vm.ts";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/**
 * The always-wired half of `HostFns`, each verb echoing what it was called with so
 * a test can assert the arguments crossed the wire intact. Nothing touches the
 * filesystem: these stand in for the real verbs, which land in T3.2/T3.3.
 */
function fakeHost(over: Partial<HostFns> = {}): HostFns {
  const echo = (label: string) => (...args: unknown[]) =>
    Promise.resolve(`${label}:${args.join("|")}`);
  return {
    bash: echo("bash"),
    sh: (cmdsJson: string) =>
      Promise.resolve(
        JSON.stringify((JSON.parse(cmdsJson) as string[]).map((c, i) => ({ code: i, out: c }))),
      ),
    bashBg: () => Promise.resolve(JSON.stringify({ id: "bg_1", pid: 4242 })),
    bashOutput: echo("bashOutput"),
    bashWait: echo("bashWait"),
    bashKill: echo("bashKill"),
    view: echo("view"),
    patch: echo("patch"),
    write: echo("write"),
    ...over,
  };
}

/** A file that exists, polled — `sh` writing it is not synchronous with our loop. */
async function waitForFile(path: string, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await stat(path);
      return true;
    } catch {
      await new Promise((r) => setTimeout(r, 25));
    }
  }
  return false;
}

const exists = async (path: string): Promise<boolean> => {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
};

// ---------------------------------------------------------------------------
// the name lists — the invariant protocol.ts exists to hold
// ---------------------------------------------------------------------------

test("host-name lists: the worker binds exactly PROGRAM_PARAMS, nothing missing", async () => {
  // Asked from INSIDE the program: `typeof x` on an undeclared identifier is
  // "undefined" rather than a ReferenceError, so a name the worker forgot to bind
  // shows up as a hole instead of blowing the program up.
  const probe = PROGRAM_PARAMS.map((n) => `[${JSON.stringify(n)}, typeof ${n}]`).join(",");
  const res = await runProgram({
    code: `console.log(JSON.stringify([${probe}]))`,
    host: fakeHost(),
  });

  ok(res.ok, res.error);
  const seen = new Map<string, string>(JSON.parse(res.logs[0]));

  strictEqual(seen.size, PROGRAM_PARAMS.length);
  for (const name of PROGRAM_PARAMS) {
    ok(seen.has(name), `${name} is in PROGRAM_PARAMS but absent from the program's scope`);
    ok(
      seen.get(name) !== "undefined",
      `${name} is declared in protocol.ts but not bound by the worker`,
    );
  }
  // Every bridged name is callable — either a function or a verb-dispatched method
  // object (`state.get`, `workflow.start`).
  for (const name of HOST_FN_NAMES) {
    ok(
      seen.get(name) === "function" || seen.get(name) === "object",
      `${name} bound as ${seen.get(name)}, which a program cannot call`,
    );
  }
  strictEqual(seen.get("console"), "object");
});

test("host-name lists: neither side re-declares the list it imports", async () => {
  // The behavioural test above proves nothing is MISSING. This one proves neither
  // side grew a second copy to drift from — three copies of the list is three
  // chances for the pre-flight check and the worker to disagree (protocol.ts).
  const here = new URL(".", import.meta.url);
  const vm = await Bun.file(new URL("vm.ts", here)).text();
  const worker = await Bun.file(new URL("vm_worker.ts", here)).text();

  for (const [name, src] of [["vm.ts", vm], ["vm_worker.ts", worker]] as const) {
    ok(src.includes('from "./protocol.ts"'), `${name} does not import the canonical list`);
    // A local array literal of host names would look like `"bash",\n  "sh",`.
    ok(
      !/["']bash["']\s*,\s*\n?\s*["']sh["']/.test(src),
      `${name} appears to declare its own host-name list`,
    );
  }
  strictEqual(worker.includes("new AsyncFunction(...PROGRAM_PARAMS"), true);
});

// ---------------------------------------------------------------------------
// pre-flight
// ---------------------------------------------------------------------------

test("pre-flight: a shadowed host name fails before a worker is spawned", async () => {
  // The host object throws on any call — reaching it would mean the program ran.
  const host = fakeHost({
    bash: () => {
      throw new Error("the program must never have started");
    },
  });
  const res = await runProgram({ code: "let bash = 1;\nawait bash('x')", host });

  strictEqual(res.ok, false);
  strictEqual(res.logs.length, 0);
  ok(res.error!.includes("does not parse"), res.error);
  // The engine's own words are carried through, whichever engine parsed: JSC says
  // "Cannot declare a let variable twice", V8 said "already been declared".
  ok(/twice|already been declared/.test(res.error!), res.error);
  // Error text is a product surface (spec §6): the message must say WHY the name is
  // taken and what to do, not just quote the parser.
  ok(res.error!.includes("already bound"), res.error);
  ok(res.error!.includes("myBash"), res.error);
});

test("pre-flight: every host name is reserved, and clean code passes", () => {
  for (const name of PROGRAM_PARAMS) {
    const msg = checkProgramSyntax(`let ${name} = 1;`);
    ok(msg, `shadowing ${name} was not caught`);
    ok(msg.includes(name), msg);
  }
  strictEqual(checkProgramSyntax("const x = await bash('ls');\nconsole.log(x)"), null);
  // Shadowing something that is NOT a host name is the program's own business.
  strictEqual(checkProgramSyntax("let notAHostFn = 1; let alsoFine = 2;"), null);
});

test("pre-flight: a newline-closed string names its line and the escaping fix", () => {
  const msg = checkProgramSyntax('const p = "one\ntwo";')!;
  ok(msg.includes("line 1"), msg);
  ok(msg.includes("consumed by the outer literal"), msg);
  // Newlines that are legal stay legal.
  strictEqual(unterminatedString("const t = `a\nb`;\n"), null);
  strictEqual(unterminatedString("// a 'quote\nconst x = 1;\n"), null);
  strictEqual(unterminatedString("/* a 'quote\nspanning */\nconst x = 1;\n"), null);
  strictEqual(unterminatedString('const s = "fine";\n'), null);
});

// ---------------------------------------------------------------------------
// results, logs, host calls
// ---------------------------------------------------------------------------

test("a throwing program surfaces its message", async () => {
  const res = await runProgram({
    code: `console.log("before"); throw new Error("boom: the thing exploded");`,
    host: fakeHost(),
  });

  strictEqual(res.ok, false);
  ok(res.error!.includes("boom: the thing exploded"), res.error);
  // Whatever it printed before dying still reaches the model.
  strictEqual(res.logs[0], "before");
  // Not an interrupt — the turn must be able to tell these apart (spec §6).
  strictEqual(res.interrupted, undefined);
});

test("a rejected host function is an ordinary catchable exception", async () => {
  const host = fakeHost({
    bash: () => Promise.reject(new Error("patch conflict at src/a.ts:74-76")),
  });
  const res = await runProgram({
    code: `try { await bash("x") } catch (e) { console.log("caught " + e.message) }`,
    host,
  });

  ok(res.ok, res.error);
  strictEqual(res.logs[0], "caught patch conflict at src/a.ts:74-76");
});

test("an unbridged host name rejects catchably and names the grant", async () => {
  // `agent` is absent from this host — absence IS the capability denial (types.ts).
  const res = await runProgram({
    code: `try { await agent("do a thing") } catch (e) { console.log("caught " + e.message) }`,
    host: fakeHost(),
  });

  ok(res.ok, res.error);
  ok(res.logs[0].includes("agent() is not available in this turn"), res.logs[0]);
  ok(res.logs[0].includes("system prompt"), res.logs[0]);
});

test("console.* both streams live and batches into the result", async () => {
  const streamed: string[] = [];
  const res = await runProgram({
    code: `
      console.log("one");
      console.error("two");
      console.warn("three");
      console.info({ a: 1 });
      console.log("multi", "part");
    `,
    host: fakeHost(),
    onLog: (line) => streamed.push(line),
  });

  ok(res.ok, res.error);
  // Same lines, same order, both paths — the stream is display-only and must not
  // change what the model receives (spec §5).
  const expected = ["one", "two", "three", '{"a":1}', "multi part"];
  strictEqual(JSON.stringify(res.logs), JSON.stringify(expected));
  strictEqual(JSON.stringify(streamed), JSON.stringify(expected));
});

test("host calls round-trip: objects out as JSON, objects back in", async () => {
  const seen: unknown[][] = [];
  const host = fakeHost({
    bash: (cmd) => {
      seen.push([cmd]);
      return Promise.resolve("ok");
    },
    ask: (question, optsJson) => {
      seen.push([question, optsJson]);
      return Promise.resolve("yes");
    },
    state: (verb, argsJson) => {
      seen.push([verb, argsJson]);
      return Promise.resolve(JSON.stringify({ verb, args: JSON.parse(argsJson) }));
    },
  });
  const res = await runProgram({
    code: `
      console.log(await bash("echo hi"));
      const shells = await sh("a", "b");
      console.log(JSON.stringify(shells));
      console.log(await ask("ok?", { options: ["y", "n"] }));
      console.log(JSON.stringify(await state.set({ key: "k", value: 1 })));
    `,
    host,
  });

  ok(res.ok, res.error);
  strictEqual(res.logs[0], "ok");
  // sh() is variadic program-side, a JSON array on the wire, and returns parsed
  // objects — a non-zero code is data.
  strictEqual(res.logs[1], JSON.stringify([{ code: 0, out: "a" }, { code: 1, out: "b" }]));
  strictEqual(res.logs[2], "yes");
  strictEqual(res.logs[3], JSON.stringify({ verb: "set", args: { key: "k", value: 1 } }));

  strictEqual(JSON.stringify(seen[0]), JSON.stringify(["echo hi"]));
  strictEqual(JSON.stringify(seen[1]), JSON.stringify(["ok?", '{"options":["y","n"]}']));
  strictEqual(JSON.stringify(seen[2]), JSON.stringify(["set", '{"key":"k","value":1}']));
});

// ---------------------------------------------------------------------------
// the exit trap
// ---------------------------------------------------------------------------

// NOTE: this used to be three tests — the same pair over `Deno.exit()` plus one
// covering `process.exit()`, the node-ism weak models reach for as an "assertion
// failed" idiom. There is no `Deno` global under Bun and no `Bun.exit`, so
// `process.exit` is the only exit there is and the third test was its duplicate.
test("process.exit() is catchable and does not kill the worker", async () => {
  const res = await runProgram({
    code: `
      try { process.exit(1) } catch (e) { console.log("caught process.exit: " + e.message) }
      console.log("still running");
    `,
    host: fakeHost(),
  });

  // The program ran to completion — an untrapped exit would have killed the worker
  // and left the turn hanging until its wall timeout (plan §6.2).
  ok(res.ok, res.error);
  ok(res.logs[0].startsWith("caught process.exit:"), res.logs[0]);
  ok(res.logs[0].includes("a program ends by returning"), res.logs[0]);
  strictEqual(res.logs[1], "still running");
});

test("an uncaught process.exit() surfaces as a program error, not a dead worker", async () => {
  const res = await runProgram({ code: `console.log("a"); process.exit(0);`, host: fakeHost() });

  strictEqual(res.ok, false);
  ok(res.error!.includes("exit(0) is not available"), res.error);
  strictEqual(res.logs[0], "a");
});

// ---------------------------------------------------------------------------
// wind-down: children first, then the worker
// ---------------------------------------------------------------------------

test("an aborted program that spawned a child leaves no orphan process", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough_vm_test_"));
  const pidFile = `${dir}/pid`;
  const marker = `${dir}/marker`;

  // The child announces itself, waits, then claims it survived. SIGTERM lands on
  // `sh` while it waits, so the marker is never written — an orphan would write it.
  const script = `echo $$ > ${pidFile}; sleep 2; echo alive > ${marker}`;
  const controller = new AbortController();
  try {
    const running = runProgram({
      code: `
        const child = Bun.spawn(["sh", "-c", ${JSON.stringify(script)}], {
          stdout: "ignore",
          stderr: "ignore",
        });
        console.log("spawned");
        await child.exited;
        console.log("child exited on its own");
      `,
      host: fakeHost(),
      signal: controller.signal,
    });

    ok(await waitForFile(pidFile, 10_000), "the child never started — nothing was tested");
    controller.abort();
    const res = await running;

    strictEqual(res.ok, false);
    // Interrupt and timeout must be distinguishable, and the message must say what
    // survived (spec §6).
    strictEqual(res.interrupted, true);
    ok(res.error!.includes("interrupted by the user"), res.error);
    ok(res.error!.includes("still stands"), res.error);
    // Streamed output survives the terminate that beat the worker's batched `logs`.
    strictEqual(res.logs[0], "spawned");
    ok(!res.logs.includes("child exited on its own"), "the program should not have finished");

    // The load-bearing assertion. `worker.terminate()` does not touch a child of the
    // SERVER process, so if the sweep ran in the wrong order — or not at all — the
    // `sleep` completes and the marker appears (plan §6.3).
    await new Promise((r) => setTimeout(r, 3_000));
    strictEqual(await exists(marker), false, "the child outlived the abort — orphaned process");
  } finally {
    controller.abort();
    await rm(dir, { recursive: true, force: true });
  }
});

test("the abort handshake sweeps children BEFORE it acks", async () => {
  // The end-to-end test above asserts the OUTCOME (no orphan), which is the thing
  // that matters — but it cannot prove WHO killed the child: the runtime also tears
  // down a terminated worker's spawned processes, so it passes even with the sweep
  // removed. This test drives the worker protocol by hand and never calls
  // `terminate()`, so the worker is still alive when the marker would be written.
  // Nothing but `killChildren()` can have stopped the child (plan §6.3).
  const dir = await mkdtemp(join(tmpdir(), "bough_vm_sweep_"));
  const pidFile = `${dir}/pid`;
  const marker = `${dir}/marker`;
  const script = `echo $$ > ${pidFile}; sleep 2; echo alive > ${marker}`;

  const worker = new Worker(new URL("./vm_worker.ts", import.meta.url).href, {
    type: "module",
  });
  try {
    let ack!: () => void;
    const acked = new Promise<void>((r) => (ack = r));
    worker.onmessage = (e: MessageEvent) => {
      if ((e.data as { type: string }).type === "aborted") ack();
    };
    worker.postMessage({
      type: "run",
      code: `
        const child = Bun.spawn(["sh", "-c", ${JSON.stringify(script)}], {
          stdout: "ignore",
          stderr: "ignore",
        });
        await child.exited;
      `,
    });

    ok(await waitForFile(pidFile, 10_000), "the child never started — nothing was tested");
    worker.postMessage({ type: "abort" });
    // The ack is the worker's promise that the sweep already ran — the host may
    // terminate only after it (protocol.ts `AbortedMessage`).
    await acked;

    await new Promise((r) => setTimeout(r, 3_000));
    strictEqual(await exists(marker), false, "the abort acked without killing the child");
  } finally {
    worker.terminate();
    await rm(dir, { recursive: true, force: true });
  }
});

test("a wall-clock timeout is reported as a timeout, not an interrupt", async () => {
  const res = await runProgram({
    code: `console.log("started"); await new Promise((r) => setTimeout(r, 30_000));`,
    host: fakeHost(),
    timeoutMs: 300,
  });

  strictEqual(res.ok, false);
  ok(res.error!.includes("timed out after 300ms"), res.error);
  // The two stop reasons must not be confusable — the turn persists one of them.
  ok(!res.error!.includes("interrupted"), res.error);
  strictEqual(res.interrupted, undefined);
  strictEqual(res.logs[0], "started");
  // Says what to do instead of a foreground wait (spec §6, plan §6.7).
  ok(res.error!.includes("bashBg"), res.error);
});

test("a signal already aborted never starts the program", async () => {
  const host = fakeHost({
    bash: () => {
      throw new Error("the program must never have started");
    },
  });
  const res = await runProgram({
    code: `await bash("echo hi")`,
    host,
    signal: AbortSignal.abort(),
  });

  strictEqual(res.ok, false);
  strictEqual(res.interrupted, true);
  strictEqual(res.logs.length, 0);
});

test("an interrupt mid-host-call still winds down and keeps partial output", async () => {
  const controller = new AbortController();
  let called = false;
  const host = fakeHost({
    // A host function that never answers — the real ones die on the turn's own
    // signal, but the bridge must not depend on that to stop.
    bash: () => {
      called = true;
      return new Promise<string>(() => {});
    },
  });

  const running = runProgram({
    code: `console.log("about to hang"); await bash("sleep 999"); console.log("unreachable");`,
    host,
    signal: controller.signal,
  });
  while (!called) await new Promise((r) => setTimeout(r, 10));
  controller.abort();
  const res = await running;

  strictEqual(res.ok, false);
  strictEqual(res.interrupted, true);
  strictEqual(res.logs[0], "about to hang");
  ok(!res.logs.includes("unreachable"));
});

/**
 * `require` is bound because weak models write CommonJS by reflex, and spec §2.2
 * already grants the program the very modules it reaches for. Before this, such a
 * program died on `ReferenceError: require is not defined` with a stack pointing
 * into `vm_worker.ts` — a message that reads as a bug in bough rather than a fixable
 * mistake in the program. A haiku run answered it by giving up on the program
 * entirely and shelling out through `bash` instead, at the cost of two extra rounds.
 */
test("a program may reach node builtins through require, not only import", async () => {
  const res = await runProgram({
    code: `
      const path = require("node:path");
      console.log(path.join("a", "b"));
    `,
    host: fakeHost(),
  });

  ok(res.ok, res.error);
  strictEqual(JSON.stringify(res.logs), JSON.stringify(["a/b"]));
});
