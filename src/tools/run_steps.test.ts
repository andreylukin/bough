import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { DONE_ACCEPTED, DONE_REJECTED, runSteps } from "./run_steps.ts";
import type { ToolRunCtx } from "./types.ts";

function ctx(): ToolRunCtx {
  return { workspace: Deno.makeTempDirSync({ prefix: "bough-runsteps-" }), turn: {} };
}

Deno.test("a program edits the workspace through host functions and reports its logs", async () => {
  const c = ctx();
  const out = await runSteps.run(
    {
      code: `await write("greet.txt", "hello");
             console.log(await bash("cat greet.txt"));`,
    },
    c,
  );
  assertStringIncludes(out, "hello");
  assertEquals(Deno.readTextFileSync(`${c.workspace}/greet.txt`), "hello");
});

Deno.test("done is gated on the committed check: fails → rejected, passes → accepted", async () => {
  const c = ctx();
  // Round 1: commit the check; the file doesn't exist yet, so done is rejected.
  const r1 = await runSteps.run(
    { code: `console.log("working");`, check: "test -f ok.txt", done: true },
    c,
  );
  assertStringIncludes(r1, DONE_REJECTED);

  // Round 2: no check re-declared (it's committed on the turn); satisfy it → accepted.
  const r2 = await runSteps.run(
    { code: `await write("ok.txt", "y");`, done: true },
    c,
  );
  assertStringIncludes(r2, DONE_ACCEPTED);
  assertEquals(c.turn?.check, "test -f ok.txt");
});

Deno.test("done with no check ever declared is accepted with a note", async () => {
  const out = await runSteps.run({ code: `console.log("hi");`, done: true }, ctx());
  assertStringIncludes(out, DONE_ACCEPTED);
  assertStringIncludes(out, "no check declared");
});

Deno.test("program errors surface in the output without killing the round", async () => {
  const out = await runSteps.run({ code: `throw new Error("boom");` }, ctx());
  assertStringIncludes(out, "[program error]");
  assertStringIncludes(out, "boom");
});

Deno.test("mcp() bridges into the program when granted; absent otherwise", async () => {
  const calls: unknown[] = [];
  const c: ToolRunCtx = {
    ...ctx(),
    mcp: {
      call: (server, tool, args) => {
        calls.push([server, tool, args]);
        if (tool === "denied") return Promise.reject(new Error("blocked by Claw Patrol"));
        return Promise.resolve({ echoed: args });
      },
    },
  };
  const out = await runSteps.run(
    {
      code: `const res = await mcp("echo", "echo", {text: "hi"});
             console.log("got", res.echoed.text);
             try { await mcp("echo", "denied", {}); } catch (e) { console.log("err:", e.message); }`,
    },
    c,
  );
  assertStringIncludes(out, "got hi");
  assertStringIncludes(out, "err:");
  assertStringIncludes(out, "blocked by Claw Patrol");
  assertEquals(calls[0], ["echo", "echo", { text: "hi" }]);

  // No grant → no host function; the call rejects inside the program.
  const bare = await runSteps.run(
    {
      code:
        `try { await mcp("echo", "echo", {}); } catch (e) { console.log("no fn:", e.message); }`,
    },
    ctx(),
  );
  assertStringIncludes(bare, "unknown host function: mcp");
});

Deno.test("mcpStatus() bridges the management state as a parsed object", async () => {
  const c: ToolRunCtx = {
    ...ctx(),
    mcpStatus: () =>
      Promise.resolve({ registry: { servers: {} }, auth: {}, active: ["exa"], connections: [] }),
  };
  const out = await runSteps.run(
    { code: `const s = await mcpStatus(); console.log("active:", s.active.join(","));` },
    c,
  );
  assertStringIncludes(out, "active: exa");

  // Not wired (e.g. a bare test ctx) → the call rejects like any unknown host fn.
  const bare = await runSteps.run(
    { code: `try { await mcpStatus(); } catch (e) { console.log("no fn:", e.message); }` },
    ctx(),
  );
  assertStringIncludes(bare, "unknown host function: mcpStatus");
});

Deno.test("lsp.* bridges verbs into the program when wired; absent otherwise", async () => {
  const calls: unknown[] = [];
  const c: ToolRunCtx = {
    ...ctx(),
    lsp: {
      call: (verb, args) => {
        calls.push([verb, args]);
        return Promise.resolve([{ name_path: "Foo/bar", relative_path: "src/foo.ts" }]);
      },
    },
  };
  const out = await runSteps.run(
    {
      code: `const hits = await lsp.refs({name_path: "Foo/bar", relative_path: "src/foo.ts"});
             console.log("first:", hits[0].name_path);`,
    },
    c,
  );
  assertStringIncludes(out, "first: Foo/bar");
  assertEquals(calls, [["refs", { name_path: "Foo/bar", relative_path: "src/foo.ts" }]]);

  // Not wired (no backing server registered) → the call rejects inside the program.
  const bare = await runSteps.run(
    { code: `try { await lsp.def({}); } catch (e) { console.log("no fn:", e.message); }` },
    ctx(),
  );
  assertStringIncludes(bare, "unknown host function: lsp");
});

Deno.test("mcp() string results round-trip too", async () => {
  const c: ToolRunCtx = { ...ctx(), mcp: { call: () => Promise.resolve("plain text") } };
  const out = await runSteps.run(
    { code: `console.log(typeof await mcp("s", "t"), await mcp("s", "t", {}));` },
    c,
  );
  assertStringIncludes(out, "string plain text");
});
