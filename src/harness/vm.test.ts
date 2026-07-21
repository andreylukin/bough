import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { type HostFns, runProgram } from "./vm.ts";

function hosts(overrides: Partial<HostFns> = {}): HostFns & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    bash: (cmd) => {
      calls.push(`bash:${cmd}`);
      return Promise.resolve(`out of ${cmd}`);
    },
    bashBg: (cmd) => {
      calls.push(`bashBg:${cmd}`);
      return Promise.resolve(`{"id":"bg_1","pid":1}`);
    },
    bashOutput: (id) => {
      calls.push(`bashOutput:${id}`);
      return Promise.resolve("(no new output)\n[running]");
    },
    bashWait: (id) => {
      calls.push(`bashWait:${id}`);
      return Promise.resolve("late\n[exited with code 0]");
    },
    bashKill: (id) => {
      calls.push(`bashKill:${id}`);
      return Promise.resolve(`sent SIGTERM to ${id}`);
    },
    read: (path) => {
      calls.push(`read:${path}`);
      return Promise.resolve("file body");
    },
    write: (path) => {
      calls.push(`write:${path}`);
      return Promise.resolve("wrote");
    },
    edit: (path) => {
      calls.push(`edit:${path}`);
      return Promise.resolve("edited");
    },
    ...overrides,
  };
}

Deno.test("program calls host functions and its console output comes back in order", async () => {
  const h = hosts();
  const res = await runProgram(
    `const files = await bash("ls");
     console.log("files:", files);
     console.log(await read("a.txt"));
     await write("b.txt", "hi");`,
    h,
  );
  assertEquals(res.ok, true);
  assertEquals(res.logs, ["files: out of ls", "file body"]);
  assertEquals(h.calls, ["bash:ls", "read:a.txt", "write:b.txt"]);
});

Deno.test("console lines stream via onLog as printed, and still batch into logs", async () => {
  const h = hosts();
  const live: string[] = [];
  const res = await runProgram(
    'console.log("one"); await bash("x"); console.log("two")',
    h,
    undefined,
    undefined,
    (line) => live.push(line),
  );
  assertEquals(live, ["one", "two"]);
  assertEquals(res, { ok: true, logs: ["one", "two"] });
});

Deno.test("lines printed before a failure stream too", async () => {
  const live: string[] = [];
  const res = await runProgram(
    'console.log("before"); throw new Error("boom")',
    hosts(),
    undefined,
    undefined,
    (line) => live.push(line),
  );
  assertEquals(live, ["before"]);
  assertEquals(res.ok, false);
  assertEquals(res.logs, ["before"]);
});

Deno.test("host-function failure rejects inside the program as a catchable exception", async () => {
  const h = hosts({ edit: () => Promise.reject(new Error("old_string not found")) });
  const res = await runProgram(
    `try { await edit("f", "a", "b"); } catch (e) { console.log("caught:", e.message); }`,
    h,
  );
  assertEquals(res.ok, true);
  assertEquals(res.logs, ["caught: old_string not found"]);
});

Deno.test("the isolate is sealed — Deno APIs are unavailable", async () => {
  const res = await runProgram(`console.log(typeof Deno);`, hosts());
  assertEquals(res.ok, true);
  // With permissions:"none" the namespace may exist but every op is denied; either
  // the type is undefined or any use throws. Assert the benign probe result only.
  const t = res.logs[0];
  assertEquals(t === "undefined" || t === "object", true);
  const escape = await runProgram(`await Deno.readTextFile("/etc/hosts");`, hosts());
  assertEquals(escape.ok, false);
});

Deno.test("a syntax/runtime error is reported, not thrown", async () => {
  const res = await runProgram(`nope.nope();`, hosts());
  assertEquals(res.ok, false);
  assertStringIncludes(res.error ?? "", "nope");
});

Deno.test("runaway program is terminated at the timeout", async () => {
  const res = await runProgram(`for (;;) {}`, hosts(), 600);
  assertEquals(res.ok, false);
  assertStringIncludes(res.error ?? "", "timed out");
});

Deno.test("interrupt: aborting the signal terminates an in-flight program promptly", async () => {
  const controller = new AbortController();
  // A host fn that never resolves — the program is stuck awaiting it, like a long
  // bash command; only the interrupt can end this before the wall-clock timeout.
  const stuck = hosts({ bash: () => new Promise<string>(() => {}) });
  const started = Date.now();
  const resultP = runProgram('await bash("sleep forever");', stuck, 60_000, controller.signal);
  setTimeout(() => controller.abort(), 50);
  const result = await resultP;
  assertEquals(result.ok, false);
  assertStringIncludes(result.error!, "interrupted");
  assertEquals(Date.now() - started < 5_000, true);
});

Deno.test("interrupt: an already-aborted signal refuses to run the program at all", async () => {
  const controller = new AbortController();
  controller.abort();
  const h = hosts();
  const result = await runProgram('await bash("x");', h, 60_000, controller.signal);
  assertEquals(result.ok, false);
  assertEquals(h.calls, []);
});
