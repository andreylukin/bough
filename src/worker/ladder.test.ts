import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { deniedShCommand, parseOps, runUnit } from "./ladder.ts";
import type { ToolRunCtx } from "../tools/types.ts";

async function tmpCtx(): Promise<ToolRunCtx> {
  return { workspace: await Deno.makeTempDir() };
}

const fence = (kind: string, arg: string, body: string) => `\`\`\`${kind} ${arg}\n${body}\n\`\`\``;

Deno.test("parseOps extracts fenced write/edit/sh blocks in order and ignores prose", () => {
  const reply = [
    "Sure! Here's the fix:",
    fence("write", "a.txt", "hello"),
    "and then",
    "```sh\necho hi\n```",
    fence("edit", "b.txt", "<<<<<<<\nold\n=======\nnew\n>>>>>>>"),
  ].join("\n");
  const ops = parseOps(reply);
  assertEquals(ops.map((o) => o.kind), ["write", "sh", "edit"]);
  assertEquals(ops[0].arg, "a.txt");
  assertEquals(ops[0].body, "hello\n");
});

Deno.test("runUnit: tier1 write that passes the check", async () => {
  const ctx = await tmpCtx();
  const result = await runUnit(
    "Create greeting.txt containing exactly: hello",
    "grep -q '^hello$' greeting.txt",
    ctx,
    { worker: () => Promise.resolve(fence("write", "greeting.txt", "hello")) },
  );
  assertEquals(result.solved, true);
  assertEquals(result.tier, "worker");
  assertEquals(result.attempts, 1);
  assertEquals(result.touched, ["greeting.txt"]);
});

Deno.test("runUnit: best-of-2 — first sample fails the check, second passes", async () => {
  const ctx = await tmpCtx();
  let calls = 0;
  const result = await runUnit(
    "Create f.txt with content: right",
    "grep -q right f.txt",
    ctx,
    {
      worker: (_s, _u, _t) => {
        calls++;
        return Promise.resolve(fence("write", "f.txt", calls === 1 ? "wrong" : "right"));
      },
    },
  );
  assertEquals(result.solved, true);
  assertEquals(result.attempts, 2);
  assertEquals(calls, 2);
});

Deno.test("runUnit: worker fails both, backstop solves", async () => {
  const ctx = await tmpCtx();
  const result = await runUnit(
    "Create f.txt with content: right",
    "grep -q right f.txt",
    ctx,
    {
      worker: () => Promise.resolve(fence("write", "f.txt", "wrong")),
      backstop: () => Promise.resolve(fence("write", "f.txt", "right")),
    },
  );
  assertEquals(result.solved, true);
  assertEquals(result.tier, "backstop");
  assertEquals(result.attempts, 3);
  // Failed attempts still report what they touched (deduped).
  assertEquals(result.touched, ["f.txt"]);
});

Deno.test("runUnit: worker unavailable goes straight to the backstop", async () => {
  const ctx = await tmpCtx();
  const result = await runUnit(
    "Create f.txt with content: right",
    "grep -q right f.txt",
    ctx,
    {
      worker: () => Promise.reject(new Error("no local worker running")),
      backstop: () => Promise.resolve(fence("write", "f.txt", "right")),
    },
  );
  assertEquals(result.solved, true);
  assertEquals(result.tier, "backstop");
  assertEquals(result.attempts, 1);
});

Deno.test("runUnit: BOUGH_WORKER_LOCAL_ONLY=1 skips the backstop", async () => {
  const ctx = await tmpCtx();
  Deno.env.set("BOUGH_WORKER_LOCAL_ONLY", "1");
  try {
    let backstopCalled = false;
    const result = await runUnit(
      "Create f.txt with content: right",
      "grep -q right f.txt",
      ctx,
      {
        worker: () => Promise.resolve(fence("write", "f.txt", "wrong")),
        backstop: () => {
          backstopCalled = true;
          return Promise.resolve(fence("write", "f.txt", "right"));
        },
      },
    );
    assertEquals(result.solved, false);
    assertEquals(result.tier, "none");
    assertEquals(backstopCalled, false);
  } finally {
    Deno.env.delete("BOUGH_WORKER_LOCAL_ONLY");
  }
});

Deno.test("runUnit: BOUGH_WORKER_FRONTIER skips the local tier — backstop is the worker", async () => {
  const ctx = await tmpCtx();
  Deno.env.set("BOUGH_WORKER_FRONTIER", "1");
  try {
    const result = await runUnit(
      "Create f.txt with content: right",
      "grep -q right f.txt",
      ctx,
      // No worker hook: frontier mode must not fall back to the local worker.
      { backstop: () => Promise.resolve(fence("write", "f.txt", "right")) },
    );
    assertEquals(result.solved, true);
    assertEquals(result.tier, "backstop");
    assertEquals(result.attempts, 1);
  } finally {
    Deno.env.delete("BOUGH_WORKER_FRONTIER");
  }
});

Deno.test("runUnit: edit op applies through edit_file, sh op runs", async () => {
  const ctx = await tmpCtx();
  await Deno.writeTextFile(`${ctx.workspace}/code.py`, "def f():\n    return 1\n");
  const reply = [
    fence("edit", "code.py", "<<<<<<<\n    return 1\n=======\n    return 2\n>>>>>>>"),
    "```sh\ntouch ran.marker\n```",
  ].join("\n");
  const result = await runUnit(
    "Make f return 2",
    "grep -q 'return 2' code.py && test -f ran.marker",
    ctx,
    { worker: () => Promise.resolve(reply) },
  );
  assertEquals(result.solved, true);
  assertEquals(await Deno.readTextFile(`${ctx.workspace}/code.py`), "def f():\n    return 2\n");
});

Deno.test("deniedShCommand flags discovery commands, including chained segments", () => {
  assertEquals(deniedShCommand("grep -r TODO ."), "grep");
  assertEquals(deniedShCommand("touch a && cat b"), "cat");
  assertEquals(deniedShCommand("echo x | sed s/x/y/"), "sed");
  assertEquals(deniedShCommand("mkdir -p x/y"), null);
  assertEquals(deniedShCommand("touch ran.marker"), null);
});

Deno.test("runUnit: sh discovery command is rejected with a corrective report", async () => {
  const ctx = await tmpCtx();
  Deno.env.set("BOUGH_WORKER_LOCAL_ONLY", "1");
  try {
    const result = await runUnit("do the thing", "true", ctx, {
      worker: () => Promise.resolve("```sh\ngrep -r TODO .\n```"),
    });
    assertEquals(result.solved, false);
    assertStringIncludes(result.report, "sh op rejected");
  } finally {
    Deno.env.delete("BOUGH_WORKER_LOCAL_ONLY");
  }
});

Deno.test("runUnit: a reply with no ops reports it and fails closed", async () => {
  const ctx = await tmpCtx();
  Deno.env.set("BOUGH_WORKER_LOCAL_ONLY", "1");
  try {
    const result = await runUnit("do the thing", "true", ctx, {
      worker: () => Promise.resolve("I think you should edit the file yourself."),
    });
    assertEquals(result.solved, false);
    assertStringIncludes(result.report, "no write/edit/sh blocks");
  } finally {
    Deno.env.delete("BOUGH_WORKER_LOCAL_ONLY");
  }
});
