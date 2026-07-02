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
