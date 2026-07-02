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
