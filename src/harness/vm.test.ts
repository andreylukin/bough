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
    sh: (cmdsJson) => {
      calls.push(`sh:${cmdsJson}`);
      const cmds = JSON.parse(cmdsJson) as string[];
      return Promise.resolve(JSON.stringify(cmds.map((c) => ({ code: 0, out: `out of ${c}` }))));
    },
    extract: (text, instruction, schemaJson) => {
      calls.push(`extract:${text}|${instruction}|${schemaJson}`);
      return Promise.resolve(JSON.stringify(`${instruction} of ${text}`));
    },
    fetch: (url, optsJson) => {
      calls.push(`fetch:${url}|${optsJson}`);
      return Promise.resolve(JSON.stringify({ status: 200, ok: true, url, body: "page" }));
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

Deno.test("the program has real permissions — Deno APIs work", async () => {
  // The host functions are convenience, not a boundary: a program that wants to
  // reach past them to the raw runtime may, and this is what makes that true.
  const res = await runProgram(
    `const t = await Deno.readTextFile("/etc/hosts"); console.log(t.length > 0);`,
    hosts(),
  );
  assertEquals(res.ok, true);
  assertEquals(res.logs, ["true"]);
});

Deno.test("an interrupt kills processes the program spawned natively", async () => {
  // worker.terminate() does not reap the program's children — they are children of
  // this process. Without the abort handshake a stopped turn leaks the build it
  // started, and the stop button lies. Pin it: the sleep must be gone afterward.
  const marker = `bough-vm-test-${crypto.randomUUID()}`;
  const ctl = new AbortController();
  const run = runProgram(
    `new Deno.Command("sh", { args: ["-c", "sleep 60 # ${marker}"] }).spawn();
     await new Promise((r) => setTimeout(r, 30_000));`,
    hosts(),
    60_000,
    ctl.signal,
  );
  // Let it get the child up before stopping.
  await new Promise((r) => setTimeout(r, 500));
  ctl.abort();
  const res = await run;
  assertEquals(res.ok, false);
  assertStringIncludes(res.error ?? "", "interrupted");

  const alive = async () =>
    (await new Deno.Command("sh", {
      args: ["-c", `ps ax | grep -F '${marker}' | grep -v grep | wc -l`],
      stdout: "piped",
    }).output()).stdout;
  // SIGTERM delivery is not instantaneous; give it a beat before judging.
  await new Promise((r) => setTimeout(r, 500));
  assertEquals(new TextDecoder().decode(await alive()).trim(), "0");
});

Deno.test("a syntax/runtime error is reported, not thrown", async () => {
  const res = await runProgram(`nope.nope();`, hosts());
  assertEquals(res.ok, false);
  assertStringIncludes(res.error ?? "", "nope");
});

Deno.test("process.exit() throws a catchable error instead of hanging the program", async () => {
  // Regression: Deno's Node-compat `process` global exists even in a
  // permissions-none worker, and exit() killed the worker silently — the
  // program promise never settled and the turn froze until its wall timeout.
  const res = await runProgram(
    `console.log("before"); process.exit(1); console.log("after");`,
    hosts(),
    5_000,
  );
  assertEquals(res.ok, false);
  assertStringIncludes(res.error ?? "", "exit(1) is not available");
  assertEquals(res.logs, ["before"]);
  // The failure idiom the guard suggests still works, and prior output survives.
  const caught = await runProgram(
    `try { process.exit(2); } catch (e) { console.log("caught:", e.message); }`,
    hosts(),
    5_000,
  );
  assertEquals(caught.ok, true);
  assertStringIncludes(caught.logs[0], "caught:");
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

Deno.test("interrupt: logs already streamed survive in the aborted result", async () => {
  const controller = new AbortController();
  // Prints, then parks on a never-resolving host call — like a long bash loop
  // that already produced output when the user hits interrupt.
  const stuck = hosts({ bash: () => new Promise<string>(() => {}) });
  const live: string[] = [];
  const result = await runProgram(
    'console.log("tick-1"); console.log("tick-2"); await bash("sleep forever");',
    stuck,
    60_000,
    controller.signal,
    (line) => {
      live.push(line);
      if (live.length === 2) controller.abort();
    },
  );
  assertEquals(result.ok, false);
  assertStringIncludes(result.error!, "interrupted");
  assertEquals(result.logs, ["tick-1", "tick-2"]);
});

Deno.test("interrupt: an already-aborted signal refuses to run the program at all", async () => {
  const controller = new AbortController();
  controller.abort();
  const h = hosts();
  const result = await runProgram('await bash("x");', h, 60_000, controller.signal);
  assertEquals(result.ok, false);
  assertEquals(h.calls, []);
});

Deno.test("extract bridges text, instruction and an optional schema, parsing the reply", async () => {
  const h = hosts();
  const result = await runProgram(
    `console.log(await extract("deno 2.1.4", "the version"));
     console.log(await extract("deno 2.1.4", "the version", {type: "object"}));`,
    h,
    60_000,
  );
  assertEquals(result.ok, true);
  assertEquals(result.logs, ["the version of deno 2.1.4", "the version of deno 2.1.4"]);
  assertEquals(h.calls, [
    "extract:deno 2.1.4|the version|null",
    `extract:deno 2.1.4|the version|{"type":"object"}`,
  ]);
});
