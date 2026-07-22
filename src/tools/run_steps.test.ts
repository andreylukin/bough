import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { DONE_ACCEPTED, DONE_REJECTED, runSteps } from "./run_steps.ts";
import { inflightForegroundOutput } from "./bash.ts";
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

Deno.test("done with no check bounces once with a nudge, then is accepted", async () => {
  const c = ctx();
  const r1 = await runSteps.run({ code: `console.log("hi");`, done: true }, c);
  assertStringIncludes(r1, "no check committed");
  const r2 = await runSteps.run({ code: `console.log("still no check");`, done: true }, c);
  assertStringIncludes(r2, DONE_ACCEPTED);
  assertStringIncludes(r2, "no check declared");
});

Deno.test("check-less done cites the last passing command in the bounce", async () => {
  const c = ctx();
  // The agent verified with a real command (exit 0) but never committed a check.
  await runSteps.run(
    { code: `await write("ok.txt", "y"); await bash("test -f ok.txt");` },
    c,
  );
  const r = await runSteps.run({ code: `console.log("done now");`, done: true }, c);
  assertStringIncludes(r, DONE_REJECTED);
  assertStringIncludes(r, "check: test -f ok.txt");
  // Mutating commands are never recorded as the candidate.
  const c2 = ctx();
  await runSteps.run({ code: `await write("a.txt", "x"); await bash("rm a.txt");` }, c2);
  const r2 = await runSteps.run({ code: `console.log("done");`, done: true }, c2);
  assertStringIncludes(r2, DONE_REJECTED);
  if (r2.includes("rm a.txt")) throw new Error("mutating command cited as check candidate");
});

Deno.test("todo commits on the turn and is echoed on every later result", async () => {
  const c = ctx();
  const r1 = await runSteps.run(
    { code: `console.log("a");`, todo: "1. rule one\n2. rule two" },
    c,
  );
  assertStringIncludes(r1, "[todo");
  assertStringIncludes(r1, "rule two");
  const r2 = await runSteps.run({ code: `console.log("b");` }, c);
  assertStringIncludes(r2, "rule one");
  const r3 = await runSteps.run({ code: `console.log("c");`, todo: "" }, c);
  assertStringIncludes(r3, "c");
  if (r3.includes("[todo")) throw new Error("cleared todo must not echo");
});

Deno.test("multi-rule requests get one spec-recheck bounce after a passing check", async () => {
  const c = ctx();
  c.turn = { requestText: "Do it.\n1. rule one\n2. rule two" };
  const r1 = await runSteps.run(
    { code: `await write("ok.txt", "y");`, check: "test -f ok.txt", done: true },
    c,
  );
  assertStringIncludes(r1, "spec recheck");
  assertStringIncludes(r1, "rule two");
  const r2 = await runSteps.run({ code: `console.log("re-verified");`, done: true }, c);
  assertStringIncludes(r2, DONE_ACCEPTED);
});

Deno.test("probe-round meter nudges after 3 no-write no-check rounds, then resets", async () => {
  const c = ctx();
  const probe = { code: `console.log("looking");` };
  // Pre-implementation exploration never trips the meter, however long.
  for (let i = 0; i < 4; i++) {
    const r = await runSteps.run(probe, c);
    if (r.includes("[verification note]")) throw new Error("nudge fired before first write");
  }
  // First write arms it; then 3 probe-only rounds nudge.
  await runSteps.run({ code: `await write("f.txt", "x");` }, c);
  const r1 = await runSteps.run(probe, c);
  const r2 = await runSteps.run(probe, c);
  if (r1.includes("[verification note]") || r2.includes("[verification note]")) {
    throw new Error("nudge fired early");
  }
  const r3 = await runSteps.run(probe, c);
  assertStringIncludes(r3, "[verification note]");
  // A writing round resets the counter.
  await runSteps.run({ code: `await write("g.txt", "y");` }, c);
  const r5 = await runSteps.run(probe, c);
  if (r5.includes("[verification note]")) throw new Error("counter did not reset");
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

Deno.test("recall() bridges semantic search when wired; absent otherwise", async () => {
  const queries: unknown[] = [];
  const c: ToolRunCtx = {
    ...ctx(),
    recall: (query, k) => {
      queries.push([query, k]);
      return Promise.resolve({
        hits: [{ sessionId: "s1", title: "old fix", snippet: "we solved it", score: 0.83 }],
        indexed: 2,
      });
    },
  };
  const out = await runSteps.run(
    {
      code: `const r = await recall("websocket fix", 5);
             console.log("hit:", r.hits[0].title, r.hits[0].score, "indexed:", r.indexed);`,
    },
    c,
  );
  assertStringIncludes(out, "hit: old fix 0.83 indexed: 2");
  assertEquals(queries, [["websocket fix", 5]]);

  // k omitted → forwarded as undefined (recall.ts applies its own default).
  await runSteps.run({ code: `await recall("just a query");` }, c);
  assertEquals(queries[1], ["just a query", undefined]);

  const bare = await runSteps.run(
    { code: `try { await recall("x"); } catch (e) { console.log("no fn:", e.message); }` },
    ctx(),
  );
  assertStringIncludes(bare, "unknown host function: recall");
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

Deno.test("artifact() bridges into the program: publishes and returns the object", async () => {
  const published: Array<{ name: string; content: string }> = [];
  const c: ToolRunCtx = {
    ...ctx(),
    artifact: (name, content) => {
      published.push({ name, content });
      return Promise.resolve({
        name,
        url: `/artifacts/S/${name}`,
        href: `http://127.0.0.1:4321/artifacts/S/${name}`,
        bytes: content.length,
        ts: 1,
      });
    },
  };
  const out = await runSteps.run(
    {
      code: `const a = await artifact("demo.html", "<h1>hi</h1>");
             console.log("href:", a.href, "bytes:", a.bytes);`,
    },
    c,
  );
  assertStringIncludes(out, "href: http://127.0.0.1:4321/artifacts/S/demo.html bytes: 11");
  assertEquals(published, [{ name: "demo.html", content: "<h1>hi</h1>" }]);

  // Not wired → the call rejects inside the program.
  const bare = await runSteps.run(
    { code: `try { await artifact("x", "y"); } catch (e) { console.log("no fn:", e.message); }` },
    ctx(),
  );
  assertStringIncludes(bare, "unknown host function: artifact");
});

Deno.test("ship() bridges into the program: options in, result object back", async () => {
  const calls: unknown[] = [];
  const c: ToolRunCtx = {
    ...ctx(),
    ship: (opts) => {
      calls.push(opts);
      return Promise.resolve({
        commit: "abc123",
        branch: "main",
        paths: ["a.txt"],
        pushed: true,
      });
    },
  };
  const out = await runSteps.run(
    {
      code: `const r = await ship({ message: "ship it", paths: ["a.txt"], push: true });
             console.log("shipped:", r.commit, r.branch, r.pushed);`,
    },
    c,
  );
  assertStringIncludes(out, "shipped: abc123 main true");
  assertEquals(calls, [{ message: "ship it", paths: ["a.txt"], push: true }]);
});

Deno.test("ask() bridges into the program: options in, chosen answer back", async () => {
  const seen: { question: string; opts?: { options?: string[] } }[] = [];
  const c: ToolRunCtx = {
    ...ctx(),
    ask: (question, opts) => {
      seen.push({ question, opts });
      return Promise.resolve("dev");
    },
  };
  const out = await runSteps.run(
    {
      code: `const a = await ask("Which env?", { options: ["dev", "prod"] });
             console.log("picked:", a);`,
    },
    c,
  );
  assertStringIncludes(out, "picked: dev");
  assertEquals(seen, [{ question: "Which env?", opts: { options: ["dev", "prod"] } }]);

  // Option-less form: the opts JSON round-trip yields an empty object.
  const free = await runSteps.run(
    { code: `console.log("typed:", await ask("Name it?"));` },
    c,
  );
  assertStringIncludes(free, "typed: dev");
  assertEquals(seen[1], { question: "Name it?", opts: {} });

  // Not wired → the call rejects inside the program.
  const bare = await runSteps.run(
    { code: `try { await ask("x"); } catch (e) { console.log("no fn:", e.message); }` },
    ctx(),
  );
  assertStringIncludes(bare, "unknown host function: ask");
});

Deno.test("a declined ask() rejects inside the program as a catchable error", async () => {
  const c: ToolRunCtx = {
    ...ctx(),
    ask: (question) => Promise.reject(new Error(`user declined to answer: ${question}`)),
  };
  const out = await runSteps.run(
    {
      code: `try { await ask("Proceed?"); } catch (e) { console.log("caught:", e.message); }
             console.log("still running");`,
    },
    c,
  );
  assertStringIncludes(out, "caught: user declined to answer: Proceed?");
  assertStringIncludes(out, "still running");
});

Deno.test("interrupted foreground bash: pre-interrupt output survives on the tool record", async () => {
  const ac = new AbortController();
  const c: ToolRunCtx = { ...ctx(), sessionId: "s-interrupt", signal: ac.signal };
  const running = runSteps.run(
    { code: `await bash("echo before-interrupt; sleep 30");` },
    c,
  );
  // Wait until the command's early output has been captured, then interrupt.
  for (let i = 0; i < 100; i++) {
    if (inflightForegroundOutput("s-interrupt")?.includes("before-interrupt")) break;
    await new Promise((r) => setTimeout(r, 50));
  }
  ac.abort();
  const out = await running;
  assertStringIncludes(out, "[program error] program interrupted by the user");
  assertStringIncludes(out, "before-interrupt");
  assertStringIncludes(out, "[interrupted] bash");
});
