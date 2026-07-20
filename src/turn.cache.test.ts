/**
 * The prompt-cache contract (see turn.ts's system/systemVolatile split and
 * llm.ts anthropicSystemBlocks): the STABLE system prefix and the tool defs must
 * be byte-identical across sessions — different workspaces, different session
 * ids — because they are the cross-session shared cache prefix. Everything
 * per-session (workspace paths etc.) must land in the VOLATILE tier, after the
 * first breakpoint. These tests drive real turns through a scripted LlmClient
 * and assert on the exact params the runner sent.
 */
import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { Db } from "./db/db.ts";
import { Bus } from "./bus.ts";
import type { Session } from "./schema/parts.ts";
import type { LlmClient, LlmParams, LlmResult } from "./supervisor/llm.ts";
import { beginTurn, type TurnCtx } from "./turn.ts";

/** A one-reply scripted client that records the params of every round; an
 * exhausted script answers the harness's stop-nudge with a stop call. */
function fakeLlm(): LlmClient & { calls: LlmParams[] } {
  let first = true;
  const calls: LlmParams[] = [];
  return {
    calls,
    run(params: LlmParams): Promise<LlmResult> {
      calls.push(params);
      const result: LlmResult = first
        ? { content: [{ type: "text", text: "ok" }], stopReason: "end_turn" }
        : {
          content: [{ type: "tool_use", id: "stop-1", name: "stop", input: {} }],
          stopReason: "tool_use",
        };
      first = false;
      return Promise.resolve(result);
    },
  };
}

/** Run one turn for a fresh root session in `workspace`; return the LLM params. */
async function turnParams(sessionId: string, workspace: string): Promise<LlmParams> {
  const db = new Db(":memory:");
  const bus = new Bus();
  const s: Session = {
    id: sessionId,
    parentId: null,
    title: sessionId,
    kind: "root",
    createdAt: 1,
  };
  db.createSession(s);
  db.createMessage({
    id: `${sessionId}-u1`,
    sessionId,
    role: "user",
    parts: [{ type: "text", text: "hi" }],
    pending: false,
    createdAt: 2,
  });
  const llm = fakeLlm();
  // No ctx.tools override: the turn uses the real default tool defs, so the
  // determinism assertion below covers what production sends.
  const ctx: TurnCtx = { db, bus, llm, workspace };
  await beginTurn(ctx, sessionId).done;
  return llm.calls[0];
}

Deno.test("cache prefix: stable system + tools byte-identical across sessions", async () => {
  const wsA = Deno.makeTempDirSync({ prefix: "bough-cache-a-" });
  const wsB = Deno.makeTempDirSync({ prefix: "bough-cache-b-" });
  const a = await turnParams("cache-sess-a", wsA);
  const b = await turnParams("cache-sess-b", wsB);

  // The shared prefix: identical bytes for different sessions AND workspaces.
  assertEquals(a.system, b.system);
  // Tool defs are part of the cached prefix — any per-session variation there
  // would break sharing just as surely as prompt text.
  assertEquals(a.tools, b.tools);
});

Deno.test("cache prefix: volatile facts never land before the first breakpoint", async () => {
  const ws = Deno.makeTempDirSync({ prefix: "bough-cache-v-" });
  const sessionId = "cache-sess-v";
  const p = await turnParams(sessionId, ws);

  // The workspace path (and any session id) lives in the volatile tier only.
  assertStringIncludes(p.systemVolatile ?? "", ws);
  assertEquals((p.system ?? "").includes(ws), false);
  assertEquals((p.system ?? "").includes(sessionId), false);
  // And the stable tier is where the base contract lives — non-empty.
  assertStringIncludes(p.system ?? "", "You are bough");
});
