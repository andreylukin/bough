import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { runOracle } from "./oracle.ts";
import type { LlmClient, LlmResult } from "../supervisor/llm.ts";

/** A scripted client: returns each result in order; records the requests. */
function scripted(results: LlmResult[]): LlmClient & { requests: unknown[] } {
  let i = 0;
  const requests: unknown[] = [];
  return {
    requests,
    run(params) {
      requests.push(params);
      return Promise.resolve(results[Math.min(i++, results.length - 1)]);
    },
  };
}

Deno.test("oracle: runs its tool loop and returns the final text answer", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-oracle-" });
  await Deno.writeTextFile(`${workspace}/a.txt`, "the answer is 42\n");
  const llm = scripted([
    {
      content: [
        { type: "text", text: "let me look" },
        { type: "tool_use", id: "t1", name: "bash", input: { command: "cat a.txt" } },
        { type: "tool_use", id: "t2", name: "read", input: { path: "a.txt" } },
      ],
      stopReason: "tool_use",
      usage: { inputTokens: 100, outputTokens: 10 },
    },
    {
      content: [{ type: "text", text: "the file says 42" }],
      stopReason: "end_turn",
      usage: { inputTokens: 200, outputTokens: 20 },
    },
  ]);
  const usage = { in: 0, out: 0 };
  try {
    const answer = await runOracle("what does a.txt say?", { workspace }, {
      model: "fake",
      llm,
      onUsage: (u) => {
        usage.in += u.inputTokens;
        usage.out += u.outputTokens;
      },
    });
    assertEquals(answer, "the file says 42");
    // Both tools executed and their results went back as the second request's input.
    const second = llm.requests[1] as { messages: { content: unknown[] }[] };
    const results = second.messages.at(-1)!.content as {
      type: string;
      content: string;
      isError: boolean;
    }[];
    assertEquals(results.length, 2);
    assert(results.every((r) => r.type === "tool_result" && !r.isError));
    assertStringIncludes(results[0].content, "the answer is 42");
    // Usage reported for every round.
    assertEquals(usage, { in: 300, out: 30 });
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});

Deno.test("oracle: a failing tool surfaces as an is_error result, not a crash", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-oracle-err-" });
  const llm = scripted([
    {
      content: [{ type: "tool_use", id: "t1", name: "read", input: { path: "missing.txt" } }],
      stopReason: "tool_use",
    },
    { content: [{ type: "text", text: "file is missing" }], stopReason: "end_turn" },
  ]);
  try {
    const answer = await runOracle("?", { workspace }, { model: "fake", llm });
    assertEquals(answer, "file is missing");
    const second = llm.requests[1] as { messages: { content: unknown[] }[] };
    const result = (second.messages.at(-1)!.content as { isError: boolean }[])[0];
    assert(result.isError, "missing file should come back as an error result");
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});

// The real read-only enforcement: the oracle's shell reads the workspace but its
// writes are denied by the Seatbelt profile. macOS-only (sandbox-exec). The
// workspace must live OUTSIDE the profile's baseline write-allow (temp, caches),
// like real workspaces do — hence $HOME, following the tools.test.ts precedent.
Deno.test({
  name: "oracle: sandboxed shell is read-only in the workspace",
  ignore: Deno.build.os !== "darwin",
  async fn() {
    const dir = `${Deno.env.get("HOME")}/bough-oracle-ro-${crypto.randomUUID()}`;
    await Deno.mkdir(dir);
    await Deno.mkdir(`${dir}/.scratch`);
    await Deno.writeTextFile(`${dir}/data.txt`, "readable\n");
    const ctx = {
      workspace: dir,
      sandbox: { sessionDir: `${dir}/.snap`, scratchDir: `${dir}/.scratch` },
    };
    const llm = scripted([
      {
        content: [
          { type: "tool_use", id: "t1", name: "bash", input: { command: "cat data.txt" } },
          { type: "tool_use", id: "t2", name: "bash", input: { command: "echo pwn > data.txt" } },
        ],
        stopReason: "tool_use",
      },
      { content: [{ type: "text", text: "done" }], stopReason: "end_turn" },
    ]);
    try {
      await runOracle("?", ctx, { model: "fake", llm });
      const second = llm.requests[1] as { messages: { content: unknown[] }[] };
      const [readRes, writeRes] = second.messages.at(-1)!.content as { content: string }[];
      assertStringIncludes(readRes.content, "readable", "read should succeed");
      assertStringIncludes(writeRes.content, "exit code", "write should be denied");
      assertEquals(await Deno.readTextFile(`${dir}/data.txt`), "readable\n");
    } finally {
      await Deno.remove(dir, { recursive: true }).catch(() => {});
    }
  },
});
