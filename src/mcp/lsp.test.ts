import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { createLspBridge, type LetaRun, lspAvailable } from "./lsp.ts";
import type { SpawnCtx } from "./manager.ts";

const spawn: SpawnCtx = { workspace: "/ws" };

/** A fake runner that records invocations and answers with canned results. */
function fakeRun(opts: { failWith?: string; failOnce?: boolean } = {}) {
  const calls: Array<{ args: string[]; cwd: string }> = [];
  let failed = false;
  const run: LetaRun = (args, cwd) => {
    calls.push({ args, cwd });
    if (opts.failWith && !(opts.failOnce && failed)) {
      failed = true;
      return Promise.resolve({ code: 1, stdout: "", stderr: opts.failWith });
    }
    return Promise.resolve({ code: 0, stdout: `${args[0]} ok`, stderr: "" });
  };
  return { run, calls };
}

Deno.test("lspAvailable: finds a leta binary on PATH", () => {
  // Only the true case is assertable: EXTRA_BIN_DIRS may find a real install
  // regardless of PATH, so "false when absent" can't be tested portably.
  const path = Deno.env.get("PATH");
  try {
    const dir = Deno.makeTempDirSync({ prefix: "bough-lsp-" });
    Deno.writeTextFileSync(`${dir}/leta`, "#!/bin/sh\n");
    Deno.env.set("PATH", dir);
    assertEquals(lspAvailable(), true);
  } finally {
    path === undefined ? Deno.env.delete("PATH") : Deno.env.set("PATH", path);
  }
});

Deno.test("bridge: first call registers the workspace once, verbs map to argv", async () => {
  const { run, calls } = fakeRun();
  const bridge = createLspBridge(spawn, run);

  const out = await bridge.call("refs", { symbol: "Gate.decide", context: 2 });
  assertEquals(out, "refs ok");
  assertEquals(calls[0], { args: ["workspace", "add"], cwd: "/ws" });
  assertEquals(calls[1], { args: ["refs", "Gate.decide", "--context", "2"], cwd: "/ws" });

  // Second call reuses the memoized registration — no new workspace add.
  await bridge.call("find", { pattern: "decide", path: "src/net" });
  assertEquals(calls.length, 3);
  assertEquals(calls[2].args, ["grep", "decide", "src/net"]);

  await bridge.call("overview", { path: "src/net/gate.ts" });
  assertEquals(calls[3].args, ["grep", ".", "src/net/gate.ts"]);
  await bridge.call("rename", { symbol: "old", new_name: "neu" });
  assertEquals(calls[4].args, ["rename", "old", "neu"]);
  await bridge.call("calls", { to: "decide" });
  assertEquals(calls[5].args, ["calls", "--to", "decide"]);
});

Deno.test("bridge: unknown verb rejects and names the verbs, without running", async () => {
  const { run, calls } = fakeRun();
  const bridge = createLspBridge(spawn, run);
  const err = await assertRejects(() => bridge.call("hover", {}), Error);
  assertStringIncludes(err.message, 'unknown lsp verb "hover"');
  assertStringIncludes(err.message, "refs");
  assertEquals(calls.length, 0);
});

Deno.test("bridge: bad args reject without running", async () => {
  const { run, calls } = fakeRun();
  const bridge = createLspBridge(spawn, run);
  await assertRejects(() => bridge.call("show", {}), Error, '"symbol"');
  await assertRejects(() => bridge.call("calls", {}), Error, 'exactly one of "to" or "from"');
  await assertRejects(
    () => bridge.call("calls", { to: "a", from: "b" }),
    Error,
    'exactly one of "to" or "from"',
  );
  assertEquals(calls.length, 0);
});

Deno.test("bridge: a failed registration surfaces and is retried next call", async () => {
  const { run, calls } = fakeRun({ failWith: "daemon dead", failOnce: true });
  const bridge = createLspBridge(spawn, run);
  const err = await assertRejects(() => bridge.call("find", { pattern: "x" }), Error);
  assertStringIncludes(err.message, "leta workspace add failed: daemon dead");
  assertEquals(calls.length, 1); // never reached the verb invocation

  // The memo was cleared: the next call registers again, then runs the verb.
  await bridge.call("find", { pattern: "x" });
  assertEquals(calls.map((c) => c.args[0]), ["workspace", "workspace", "grep"]);
});

Deno.test("bridge: a failed verb surfaces stderr in the error", async () => {
  const calls: Array<string[]> = [];
  const run: LetaRun = (args) => {
    calls.push(args);
    return Promise.resolve(
      args[0] === "workspace"
        ? { code: 0, stdout: "", stderr: "" }
        : { code: 1, stdout: "", stderr: "Error: Symbol 'nope' not found" },
    );
  };
  const bridge = createLspBridge(spawn, run);
  const err = await assertRejects(() => bridge.call("show", { symbol: "nope" }), Error);
  assertStringIncludes(err.message, "lsp.show failed: Error: Symbol 'nope' not found");
});
