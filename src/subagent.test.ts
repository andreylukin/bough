/**
 * Subagent integration: a supervisor program calls agent() (through run_steps and
 * the sealed VM) and a real subagent session spins up as a tree branch. It shares
 * the spawner's workspace — the user's checkout — so its writes are simply there;
 * there is no branch to adopt. The LLM is a dispatcher keyed by the thread's first
 * user text, because spawner and subagent turns interleave on the same injected
 * client.
 */
import { assertEquals, assertExists, assertStringIncludes, assertThrows } from "jsr:@std/assert@1";
import { Db } from "./db/db.ts";
import { Bus } from "./bus.ts";
import type { Message, Part, Session } from "./schema/parts.ts";
import type { TurnStatus } from "./db/db.ts";
import type { LlmClient, LlmParams, LlmResult } from "./supervisor/llm.ts";
import { defaultTools } from "./tools/mod.ts";
import { beginTurn, interruptTurn, startUserTurn, type TurnCtx } from "./turn.ts";
import { buildResult, cleanSubagentName, taskStubTitle } from "./subagent.ts";
import { saveRegistry, setActivation } from "./mcp/config.ts";
import { mcpManager } from "./mcp/manager.ts";

// ---- harness ---------------------------------------------------------------

/**
 * Scripted rounds per conversation, keyed by the LAST text-bearing user message
 * (tool_result-only user messages don't carry text) — so a continued thread
 * dispatches on its newest human message, not the original task. A round may be a
 * thunk returning a promise, to gate WHEN that thread's reply lands (background
 * subagent timing).
 */
// A thunk receives the round's abort signal, so a failure/interrupt scenario can
// reject when the turn is aborted (simulating a real LLM's aborted request).
type ScriptedRound = LlmResult | ((signal?: AbortSignal) => Promise<LlmResult>);
function dispatchLlm(scripts: Record<string, ScriptedRound[]>): LlmClient {
  const idx: Record<string, number> = {};
  return {
    async run(
      params: LlmParams,
      onText: (d: string) => void,
      signal?: AbortSignal,
    ): Promise<LlmResult> {
      const text = [...params.messages].reverse()
        .filter((m) => m.role === "user")
        .map((m) =>
          (m.content.find((b) => b.type === "text") as { text?: string } | undefined)?.text
        )
        .find((t) => t !== undefined) ?? "";
      // The harness's stop-nudge (in-memory re-prompt after a text-only round)
      // gets the compliant reply — a stop call — instead of a script lookup.
      if (text.startsWith("[harness]")) {
        return {
          content: [{
            type: "tool_use",
            id: `stop-${crypto.randomUUID().slice(0, 8)}`,
            name: "stop",
            input: {},
          }],
          stopReason: "tool_use",
        };
      }
      const key = Object.keys(scripts).find((k) => text.startsWith(k));
      if (!key) throw new Error(`no script for thread starting with: ${text.slice(0, 60)}`);
      const i = idx[key] ?? 0;
      idx[key] = i + 1;
      const scripted = scripts[key][i];
      if (!scripted) throw new Error(`script "${key}" exhausted at round ${i + 1}`);
      const result = typeof scripted === "function" ? await scripted(signal) : scripted;
      for (const block of result.content) {
        if (block.type === "text") onText(block.text);
      }
      return result;
    },
  };
}

/** Poll until `cond` holds (turns run fire-and-forget; wakes have no handle to await). */
async function until(cond: () => boolean, ms = 5000): Promise<void> {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error("timed out waiting for condition");
    await new Promise((r) => setTimeout(r, 20));
  }
}

function program(code: string, extra: Record<string, unknown> = {}): LlmResult {
  return {
    content: [{
      type: "tool_use",
      id: `tu-${crypto.randomUUID().slice(0, 8)}`,
      name: "run_steps",
      input: { code, ...extra },
    }],
    stopReason: "tool_use",
  };
}

function textRound(text: string): LlmResult {
  return { content: [{ type: "text", text }], stopReason: "end_turn" };
}

function seed(db: Db, workspace?: string): Session {
  const s: Session = {
    id: "s1",
    parentId: null,
    title: "spawner",
    kind: "root",
    createdAt: 1,
    ...(workspace ? { workspace } : {}),
  };
  db.createSession(s);
  db.createMessage({
    id: "u1",
    sessionId: s.id,
    role: "user",
    parts: [{ type: "text", text: "hi" }],
    pending: false,
    createdAt: 2,
  });
  return s;
}

function lastToolResult(m: Message): string {
  const tr = m.parts.filter((p) => p.type === "tool_result").at(-1);
  assertExists(tr);
  return String((tr as { output: unknown }).output);
}

async function sh(bin: string, args: string[], cwd: string): Promise<void> {
  const { code, stderr } = await new Deno.Command(bin, {
    args,
    cwd,
    stdout: "null",
    stderr: "piped",
  }).output();
  if (code !== 0) throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(stderr)}`);
}

async function tempGitRepo(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "subagent-test-" });
  await sh("git", ["init", "-q", "."], dir);
  await Deno.writeTextFile(`${dir}/README.md`, "base\n");
  await sh("git", ["add", "-A"], dir);
  await sh("git", [
    "-c",
    "user.email=t@t",
    "-c",
    "user.name=t",
    "-c",
    "commit.gpgsign=false",
    "commit",
    "-qm",
    "init",
  ], dir);
  return dir;
}

async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}

const gitAvailable = await canRun("git");

// ---- tests -----------------------------------------------------------------

Deno.test("agent() spawns a subagent branch and the program receives its report", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(`const r = await agent("say hello"); console.log("GOT:" + r.report + "|" + r.ok);`),
      textRound("delegated fine"),
    ],
    "say hello": [textRound("hello from sub")],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, spawner.id);
  await done;

  // The program saw the subagent's report and completion flag.
  const final = db.getMessage(message.id)!;
  assertStringIncludes(lastToolResult(final), "GOT:hello from sub|true");

  // The subagent is a real session: kind subagent, fresh thread (parentId null),
  // lineage pointing at the spawning turn — exactly what the map draws a branch from.
  const sub = db.listSessions().find((s) => s.kind === "subagent");
  assertExists(sub);
  assertEquals(sub.parentId, null);
  assertEquals(sub.originId, spawner.id);
  assertEquals(sub.originMessageId, message.id);

  // Its thread is task + reply, finished — continuable like any session.
  const thread = db.threadFor(sub.id);
  assertEquals(thread.length, 2);
  assertEquals(thread[0].role, "user");
  assertEquals(thread[1].pending, false);

  // The title worker names the branch from its task (not a raw task-prefix).
  await until(() => db.getSession(sub.id)!.title.startsWith("titled:"));
});

Deno.test("agent(task, {name}) names the branch, and the title worker leaves it alone", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(
        `const r = await agent("audit the seatbelt profile end to end", ` +
          `{name: "audit seatbelt"}); console.log("GOT:" + r.ok);`,
      ),
      textRound("delegated fine"),
    ],
    "audit the seatbelt profile end to end": [textRound("audited")],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { done } = beginTurn(ctx, spawner.id);
  await done;

  const sub = db.listSessions().find((s) => s.kind === "subagent");
  assertExists(sub);
  // The caller's name IS the title — not the task's first 40 characters.
  assertEquals(db.getSession(sub.id)!.title, "audit seatbelt");
  // And the title worker must not rename it out from under the spawner: give the
  // fire-and-forget path a chance to run, then assert it did not fire.
  await new Promise((r) => setTimeout(r, 50));
  assertEquals(db.getSession(sub.id)!.title, "audit seatbelt");
});

Deno.test("spawn() is non-blocking: the spawner's turn ends, the finished report wakes it", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  // The subagent's reply is gated so it finishes only AFTER the spawner's turn is
  // over — the exact background scenario: spawn, end turn, get woken by the note.
  let releaseSub!: () => void;
  const gate = new Promise<void>((r) => (releaseSub = r));
  const llm = dispatchLlm({
    "hi": [
      program(
        `const h = await spawn("background research"); console.log("spawned:" + (h.sessionId ? "yes" : "no"));`,
      ),
      textRound("turn over, working on other things"),
    ],
    "background research": [
      async () => {
        await gate;
        return textRound("bg findings: all good");
      },
    ],
    "[subagent finished]": [textRound("noted the findings")],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, spawner.id);
  await done;

  // The spawner's turn completed while the subagent is still running.
  const spawnerMsg = db.getMessage(message.id)!;
  assertEquals(spawnerMsg.pending, false);
  assertStringIncludes(lastToolResult(spawnerMsg), "spawned:yes");
  const sub = db.listSessions().find((s) => s.kind === "subagent")!;
  assertEquals(db.messagesFor(sub.id).at(-1)!.pending, true); // still working

  // Let the subagent finish → its report lands as a system note and wakes the spawner.
  releaseSub();
  await until(() => {
    const own = db.messagesFor(spawner.id);
    return own.some((m) => m.role === "system") &&
      own.at(-1)!.role === "supervisor" && !own.at(-1)!.pending;
  });
  const own = db.messagesFor(spawner.id);
  const note = own.find((m) => m.role === "system")!;
  const noteText = (note.parts[0] as { text: string }).text;
  assertStringIncludes(noteText, "[subagent finished]");
  assertStringIncludes(noteText, "bg findings: all good");
  // The wake turn actually ran and saw the note.
  const wake = own.at(-1)!;
  assertStringIncludes((wake.parts.at(-1) as { text: string }).text, "noted the findings");
});

Deno.test("join() claims a background result in-band — no wake note is posted", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  let releaseSub!: () => void;
  const gate = new Promise<void>((r) => (releaseSub = r));
  // Release the gate only once the spawner program is (deterministically) inside
  // join(): the join host call registers its claim synchronously, so releasing on
  // the next macrotask after the program logs "joining" is race-free enough here —
  // the claim happens before the gated LLM round can resolve.
  const llm = dispatchLlm({
    "hi": [
      program(
        `const h = await spawn("bg join task");
         const r = await join(h.sessionId);
         console.log("JOINED:" + r.report + "|" + r.ok);`,
      ),
      textRound("done"),
    ],
    "bg join task": [
      async () => {
        await gate;
        return textRound("join report");
      },
    ],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, spawner.id);
  // Give the program time to reach join() (claim registered), then finish the sub.
  await until(() => db.listSessions().some((s) => s.kind === "subagent"));
  await new Promise((r) => setTimeout(r, 50));
  releaseSub();
  await done;

  assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "JOINED:join report|true");
  // Claimed in-band → no system note, no wake turn.
  assertEquals(db.messagesFor(spawner.id).some((m) => m.role === "system"), false);
});

/** Seed a subagent chain root → sub1 (→ sub2), returning nothing; ids are fixed. */
function seedChain(db: Db, depth: 1 | 2): void {
  db.createSession({ id: "root", parentId: null, title: "root", kind: "root", createdAt: 1 });
  db.createSession({
    id: "sub1",
    parentId: null,
    title: "subagent",
    kind: "subagent",
    createdAt: 2,
    originId: "root",
    originMessageId: "m0",
  });
  if (depth === 2) {
    db.createSession({
      id: "sub2",
      parentId: null,
      title: "nested subagent",
      kind: "subagent",
      createdAt: 3,
      originId: "sub1",
      originMessageId: "m1",
    });
  }
}

Deno.test("a subagent can agent() one level down; the nested branch points back at it", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  seedChain(db, 1);
  db.createMessage({
    id: "u1",
    sessionId: "sub1",
    role: "user",
    parts: [{ type: "text", text: "delegate a piece" }],
    pending: false,
    createdAt: 3,
  });
  const llm = dispatchLlm({
    "delegate a piece": [
      program(`const r = await agent("nested task"); console.log("GOT:" + r.report + "|" + r.ok);`),
      textRound("nested delegation worked"),
    ],
    "nested task": [textRound("hello from depth 2")],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, "sub1");
  await done;

  assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "GOT:hello from depth 2|true");
  const nested = db.listSessions().find((s) => s.kind === "subagent" && s.originId === "sub1");
  assertExists(nested);
  assertEquals(nested.originMessageId, message.id);
});

Deno.test("subagents get blocking delegation only (no spawn/join host functions)", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  seedChain(db, 1);
  db.createMessage({
    id: "u1",
    sessionId: "sub1",
    role: "user",
    parts: [{ type: "text", text: "try to spawn" }],
    pending: false,
    createdAt: 3,
  });
  const llm = dispatchLlm({
    "try to spawn": [
      program(`const h = await spawn("detached"); console.log(h);`),
      textRound("could not"),
    ],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, "sub1");
  await done;

  assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "unknown host function: spawn");
  assertEquals(db.listSessions().filter((s) => s.kind === "subagent").length, 1);
});

Deno.test("spawn cap: the 9th spawn from one turn fails; the model sees the error", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(
        `for (let i = 0; i < 9; i++) {
           try { await agent("cap task " + i); } catch (e) { console.log("REFUSED at " + i + ": " + e.message); }
         }`,
      ),
      textRound("done"),
    ],
    "cap task": [
      textRound("ok"),
      textRound("ok"),
      textRound("ok"),
      textRound("ok"),
      textRound("ok"),
      textRound("ok"),
      textRound("ok"),
      textRound("ok"),
    ],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, spawner.id);
  await done;

  const out = lastToolResult(db.getMessage(message.id)!);
  assertStringIncludes(out, "REFUSED at 8: spawn cap reached");
  assertEquals(db.listSessions().filter((s) => s.kind === "subagent").length, 8);
});

Deno.test("concurrency cap: a 5th parallel spawn is refused while 4 run", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  // Four subagents block on a gate; the 5th spawn must be refused, then the gate
  // opens and the four finish normally.
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const gated = () => async () => {
    await gate;
    return textRound("finished");
  };
  const llm = dispatchLlm({
    "hi": [
      program(
        `const four = [0, 1, 2, 3].map((i) => agent("parallel task " + i));
         await new Promise((r) => setTimeout(r, 200)); // let all four start
         try { await agent("one too many"); } catch (e) { console.log("REFUSED: " + e.message); }
         console.log("release");
         await bash("true"); // reach the host boundary so the log flushes deterministically
         await Promise.all(four);
         console.log("ALL DONE");`,
      ),
      textRound("done"),
    ],
    "parallel task": [gated(), gated(), gated(), gated()],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, spawner.id);
  // Wait for the refusal, then let the four gated subagents finish.
  await until(() => db.listSessions().filter((s) => s.kind === "subagent").length === 4);
  await new Promise((r) => setTimeout(r, 300));
  release();
  await done;

  const out = lastToolResult(db.getMessage(message.id)!);
  assertStringIncludes(out, "REFUSED: subagent concurrency cap reached");
  assertStringIncludes(out, "ALL DONE");
  assertEquals(db.listSessions().filter((s) => s.kind === "subagent").length, 4);
});

Deno.test("delegation stops at the depth cap (no agent() host function at depth 2)", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  seedChain(db, 2);
  db.createMessage({
    id: "u1",
    sessionId: "sub2",
    role: "user",
    parts: [{ type: "text", text: "try to delegate" }],
    pending: false,
    createdAt: 4,
  });
  const llm = dispatchLlm({
    "try to delegate": [
      program(`const r = await agent("nested"); console.log(r);`),
      textRound("could not"),
    ],
  });
  const ctx: TurnCtx = {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
  };

  const { message, done } = beginTurn(ctx, "sub2");
  await done;

  assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "unknown host function: agent");
  assertEquals(db.listSessions().filter((s) => s.kind === "subagent").length, 2);
});

/** Point every session's snapshot/scratch state at temp dirs for `fn`. The
 *  workspace is NOT redirected — that's the point: the turn runs in `repo`. */
async function withTempState(fn: () => Promise<void>): Promise<void> {
  const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
  const scratchBase = await Deno.makeTempDir({ prefix: "subagent-scratch-" });
  const prev = new Map<string, string | undefined>([
    ["BOUGH_SNAPSHOT_BASE", Deno.env.get("BOUGH_SNAPSHOT_BASE")],
    ["BOUGH_SCRATCH_BASE", Deno.env.get("BOUGH_SCRATCH_BASE")],
  ]);
  Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
  Deno.env.set("BOUGH_SCRATCH_BASE", scratchBase);
  try {
    await fn();
  } finally {
    for (const [k, v] of prev) v === undefined ? Deno.env.delete(k) : Deno.env.set(k, v);
    await Deno.remove(snapBase, { recursive: true }).catch(() => {});
    await Deno.remove(scratchBase, { recursive: true }).catch(() => {});
  }
}

Deno.test({
  // The nested chain on a repo: spawner, subagent and grandchild all work the ONE
  // checkout, so a write two tiers down is on disk immediately and adopt() has
  // nothing to move — it says so rather than pretending it merged a branch.
  name: "nested repo delegation: the whole chain works the user's checkout in place",
  ignore: !gitAvailable,
  fn: () =>
    withTempState(async () => {
      const repo = await tempGitRepo();
      const db = new Db(":memory:");
      const bus = new Bus();
      try {
        const spawner = seed(db, repo);
        const llm = dispatchLlm({
          "hi": [
            program(
              `const r = await agent("orchestrate: have nested.txt created");
               console.log("subchanged:" + JSON.stringify(r.changedFiles));
               console.log(await adopt(r.sessionId));`,
            ),
            textRound("all done"),
          ],
          "orchestrate": [
            program(
              `const g = await agent("write nested.txt containing from-nested");
               console.log("grandchild:" + JSON.stringify(g.changedFiles));
               console.log(await adopt(g.sessionId));`,
              { done: true },
            ),
          ],
          "write nested.txt": [
            program(`await write("nested.txt", "from-nested\\n"); console.log("wrote");`, {
              done: true,
            }),
          ],
        });
        const ctx: TurnCtx = {
          db,
          bus,
          llm,
          tools: defaultTools,
          titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
        };

        const { message, done } = beginTurn(ctx, spawner.id);
        await done;

        const out = lastToolResult(db.getMessage(message.id)!);
        // Each tier's changedFiles is read off the shared tree, so the grandchild's
        // write shows up at every level — and adopt is a no-op explainer.
        assertStringIncludes(out, 'subchanged:["nested.txt"]');
        assertStringIncludes(out, "nothing to adopt");
        assertEquals(await Deno.readTextFile(`${repo}/nested.txt`), "from-nested\n");

        // Three tiers of sessions; the grandchild's lineage points at the subagent…
        const subs = db.listSessions().filter((s) => s.kind === "subagent");
        assertEquals(subs.length, 2);
        const sub = subs.find((s) => s.originId === spawner.id)!;
        const grandchild = subs.find((s) => s.originId === sub.id)!;
        assertExists(grandchild);
        // …and every tier ran in the same dir: no per-session working copies.
        assertEquals(db.getSessionRuntime(sub.id).workspace, repo);
        assertEquals(db.getSessionRuntime(grandchild.id).workspace, repo);
      } finally {
        db.close();
        await Deno.remove(repo, { recursive: true }).catch(() => {});
      }
    }),
});

Deno.test({
  // "Continue off of it": a subagent is a plain session — a human message posted to
  // it (the same startUserTurn path behind POST /sessions/:id/messages) runs a new
  // turn, and because it shares the spawner's checkout the follow-up edit lands
  // there too, alongside the first one.
  name: "a finished subagent accepts human messages and keeps working the shared checkout",
  ignore: !gitAvailable,
  fn: () =>
    withTempState(async () => {
      const repo = await tempGitRepo();
      const db = new Db(":memory:");
      const bus = new Bus();
      try {
        const spawner = seed(db, repo);
        const llm = dispatchLlm({
          "hi": [
            program(`const r = await agent("make sub.txt please"); console.log(r.ok);`),
            textRound("spawned"),
          ],
          "make sub.txt": [
            program(`await write("sub.txt", "v1\\n"); console.log("ok");`, { done: true }),
          ],
          "also add more.txt": [
            program(`await write("more.txt", "v2\\n"); console.log("ok");`, { done: true }),
          ],
        });
        const ctx: TurnCtx = {
          db,
          bus,
          llm,
          tools: defaultTools,
          titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
        };

        const { done } = beginTurn(ctx, spawner.id);
        await done;
        const sub = db.listSessions().find((s) => s.kind === "subagent")!;

        // A human continues the subagent — same path the composer/API uses.
        const { done: continued } = startUserTurn(ctx, sub.id, "also add more.txt");
        await continued;

        // The follow-up turn ran on the subagent's thread…
        const own = db.messagesFor(sub.id);
        assertEquals(own.length, 4); // task, reply, human follow-up, reply
        assertEquals(own[3].pending, false);
        // …and both of its edits are in the one checkout the session works.
        assertEquals(await Deno.readTextFile(`${repo}/sub.txt`), "v1\n");
        assertEquals(await Deno.readTextFile(`${repo}/more.txt`), "v2\n");
        assertEquals(db.getSessionRuntime(sub.id).workspace, repo);
      } finally {
        db.close();
        await Deno.remove(repo, { recursive: true }).catch(() => {});
      }
    }),
});

Deno.test({
  // Parallel fan-out shares one tree, which is exactly why the delegation prompt
  // tells the model to give siblings disjoint files: both writes land for real,
  // and nothing merges them afterwards.
  name: "parallel subagents: two agent() calls in Promise.all both land in the checkout",
  ignore: !gitAvailable,
  fn: () =>
    withTempState(async () => {
      const repo = await tempGitRepo();
      const db = new Db(":memory:");
      const bus = new Bus();
      try {
        const spawner = seed(db, repo);
        const llm = dispatchLlm({
          "hi": [
            program(
              `const [a, b] = await Promise.all([
                 agent("task alpha: create a.txt"),
                 agent("task beta: create b.txt"),
               ]);
               console.log("a:" + JSON.stringify(a.changedFiles) + " b:" + JSON.stringify(b.changedFiles));`,
            ),
            textRound("both done"),
          ],
          "task alpha": [
            program(`await write("a.txt", "alpha\\n"); console.log("ok");`, { done: true }),
          ],
          "task beta": [
            program(`await write("b.txt", "beta\\n"); console.log("ok");`, { done: true }),
          ],
        });
        const ctx: TurnCtx = {
          db,
          bus,
          llm,
          tools: defaultTools,
          titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
        };

        const { message, done } = beginTurn(ctx, spawner.id);
        await done;

        const out = lastToolResult(db.getMessage(message.id)!);
        assertStringIncludes(out, "a.txt");
        assertStringIncludes(out, "b.txt");

        // Two subagent lanes, one workspace; both files are really there.
        const subs = db.listSessions().filter((s) => s.kind === "subagent");
        assertEquals(subs.length, 2);
        for (const sub of subs) assertEquals(db.getSessionRuntime(sub.id).workspace, repo);
        assertEquals(await Deno.readTextFile(`${repo}/a.txt`), "alpha\n");
        assertEquals(await Deno.readTextFile(`${repo}/b.txt`), "beta\n");
      } finally {
        db.close();
        await Deno.remove(repo, { recursive: true }).catch(() => {});
      }
    }),
});

Deno.test({
  // MCP inheritance: the spawning turn's grant (here a manual activation for the
  // spawner session) carries over to the subagent turn — its program gets a working
  // mcp() bridged to the same servers, connected under the SUBAGENT's session id.
  name: "subagent turns inherit the spawner's MCP grant",
  fn: async () => {
    if ((await Deno.permissions.query({ name: "run" })).state !== "granted") return;
    const mcpDir = await Deno.makeTempDir({ prefix: "subagent-mcp-" });
    const prevMcp = Deno.env.get("BOUGH_MCP_DIR");
    Deno.env.set("BOUGH_MCP_DIR", mcpDir);
    const db = new Db(":memory:");
    const bus = new Bus();
    try {
      const fixture = new URL("./mcp/testdata/echo_server.ts", import.meta.url).pathname;
      saveRegistry({
        servers: {
          echo: { command: Deno.execPath(), args: ["run", "--quiet", "--no-config", fixture] },
        },
      });
      const spawner = seed(db);
      setActivation(spawner.id, "echo", true);
      const llm = dispatchLlm({
        "hi": [
          program(`const r = await agent("probe the echo server"); console.log("sub-ok:" + r.ok);`),
          textRound("delegated"),
        ],
        "probe the echo server": [
          program(
            `const out = await mcp("echo", "echo", { text: "from-sub" });
             console.log("MCP:" + JSON.stringify(out));`,
            // A committed check lets this single round finish (status=done → ok:true);
            // the point under test is that mcp() round-trips, which the log proves.
            { done: true, check: "true" },
          ),
        ],
      });
      const ctx: TurnCtx = {
        db,
        bus,
        llm,
        tools: defaultTools,
        titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)),
      };

      const { message, done } = beginTurn(ctx, spawner.id);
      await done;

      // The subagent's turn completed with a working mcp() …
      assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "sub-ok:true");
      // …and its program's output shows the real round-trip through the server.
      const sub = db.listSessions().find((s) => s.kind === "subagent")!;
      const subReply = db.messagesFor(sub.id).find((m) => m.role === "supervisor")!;
      assertStringIncludes(lastToolResult(subReply), 'MCP:{"echoed":"from-sub"}');
    } finally {
      await mcpManager().dropAll();
      if (prevMcp === undefined) Deno.env.delete("BOUGH_MCP_DIR");
      else Deno.env.set("BOUGH_MCP_DIR", prevMcp);
      await Deno.remove(mcpDir, { recursive: true }).catch(() => {});
    }
  },
});

// ---- failure & interruption edge cases -------------------------------------
// A subagent can fail (its turn errors), be interrupted (user stop, or the
// spawner's interrupt cascading), time out, or fail to even launch (caps, bad
// task). This block exercises how each outcome flows back — the surface behind
// the "subagent failed / was interrupted" UX. Caps (spawn/concurrency/depth) are
// covered above; here we cover error, interrupt, timeout, and partial fan-out.

/** A round that rejects like a real LLM request error. */
function errorRound(message: string): ScriptedRound {
  return () => Promise.reject(new Error(message));
}

/** A round that never resolves until its turn is aborted, then rejects like an
 * aborted LLM request — for interrupt/timeout scenarios. */
function abortRound(onStart?: () => void): ScriptedRound {
  return (signal?: AbortSignal) =>
    new Promise<LlmResult>((_res, rej) => {
      onStart?.();
      if (signal?.aborted) return rej(new DOMException("aborted", "AbortError"));
      signal?.addEventListener("abort", () => rej(new DOMException("aborted", "AbortError")), {
        once: true,
      });
    });
}

function failCtx(db: Db, bus: Bus, llm: LlmClient): TurnCtx {
  return {
    db,
    bus,
    llm,
    tools: defaultTools,
    titler: (t) => Promise.resolve("t:" + t.slice(0, 12)),
  };
}

Deno.test("A1 blocking agent(): a subagent whose turn errors returns ok:false with the error in the report", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(
        `const r = await agent("do the thing"); console.log("ok=" + r.ok + " report=" + r.report);`,
      ),
      textRound("handled the failure"),
    ],
    "do the thing": [errorRound("model exploded mid-turn")],
  });
  const { message, done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await done;

  const out = lastToolResult(db.getMessage(message.id)!);
  assertStringIncludes(out, "ok=false");
  assertStringIncludes(out, "Turn failed"); // the subagent's error is carried in its report
  assertStringIncludes(out, "model exploded mid-turn");
  // The subagent session is a real, finished branch with error status.
  const sub = db.listSessions().find((s) => s.kind === "subagent")!;
  const subMsg = db.messagesFor(sub.id).at(-1)!;
  assertEquals(subMsg.pending, false);
  // The outcome is persisted on the branch row — the TUI renders "✗ failed" from
  // it (the in-band result above only reached the spawner's program).
  assertEquals(sub.outcomeOk, false);
  assertEquals(sub.outcomeCheckPassed, false);
});

Deno.test("A2 detached spawn(): a subagent that errors posts a FAILED completion note that wakes the spawner", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(`await spawn("bg that dies"); console.log("spawned");`),
      textRound("turn over"),
    ],
    "bg that dies": [errorRound("boom in background")],
    "[subagent finished]": [textRound("saw the failure note")],
  });
  const { done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await done;

  await until(() => db.messagesFor(spawner.id).some((m) => m.role === "system"));
  const note = db.messagesFor(spawner.id).find((m) => m.role === "system")!;
  const text = (note.parts[0] as { text: string }).text;
  assertStringIncludes(text, "[subagent finished]");
  // The note says WHY: an errored turn reads as FAILED — its turn errored, and
  // the error itself is in the report (distinct from a stop/timeout/orphan).
  assertStringIncludes(text, "FAILED — its turn errored");
  assertStringIncludes(text, "boom in background");
});

Deno.test("A4 partial success: a subagent that finishes without passing a check is ok:true but checkPassed:false", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(
        `const r = await agent("task"); console.log("ok=" + r.ok + " check=" + r.checkPassed);`,
      ),
      textRound("done"),
    ],
    // finishes normally (text reply, no committed check / done gate)
    "task": [textRound("I did some of it")],
  });
  const { message, done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await done;
  const out = lastToolResult(db.getMessage(message.id)!);
  // "finished but unverified" must be distinguishable from a hard failure.
  assertStringIncludes(out, "ok=true");
  assertStringIncludes(out, "check=false");
  // Persisted on the branch row too — the TUI's check-failed card reads these.
  const sub = db.listSessions().find((s) => s.kind === "subagent")!;
  assertEquals(sub.outcomeOk, true);
  assertEquals(sub.outcomeCheckPassed, false);
});

Deno.test("A5 full success: a subagent that commits done persists ok:true + checkPassed:true on its row", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(
        `const r = await agent("task"); console.log("ok=" + r.ok + " check=" + r.checkPassed);`,
      ),
      textRound("done"),
    ],
    // The first check-less done bounces (check nudge); the second commits a real
    // check that passes, so the harness accepts via the "— check passed" path —
    // the ONLY path that earns a green checkPassed:true (an unchecked done is
    // "— no check declared", which is not-verified/amber).
    "task": [
      program(`console.log("did it");`, { done: true }),
      program(`console.log("still done");`, { done: true, check: "true" }),
    ],
  });
  const { message, done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await done;
  const out = lastToolResult(db.getMessage(message.id)!);
  assertStringIncludes(out, "ok=true");
  assertStringIncludes(out, "check=true");
  // The only combination the TUI may paint green "✓ done".
  const sub = db.listSessions().find((s) => s.kind === "subagent")!;
  assertEquals(sub.outcomeOk, true);
  assertEquals(sub.outcomeCheckPassed, true);
});

Deno.test("B1 interrupt cascades: interrupting the spawner while agent() blocks interrupts the subagent (ok:false)", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  let subStarted = false;
  const llm = dispatchLlm({
    "hi": [
      program(`const r = await agent("slow work"); console.log("ok=" + r.ok);`),
      textRound("stopped"),
    ],
    "slow work": [abortRound(() => (subStarted = true))],
  });
  const { message, done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await until(() => subStarted);
  interruptTurn(spawner.id); // user stop on the spawner
  await done;

  // Both stop. The subagent's turn ends interrupted...
  const sub = db.listSessions().find((s) => s.kind === "subagent")!;
  const t = db.turnForMessage(db.messagesFor(sub.id).at(-1)!.id);
  assertEquals(t?.status, "interrupted");
  // ...and the spawner's PROGRAM is killed by the same interrupt — so it never
  // gets to observe the subagent's result (no graceful "ok=false" handling). The
  // tool_result is the program-interrupt notice, and the spawner turn is
  // interrupted. This pins the real cascade: interrupt = stop everything now.
  assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "interrupted");
  const spawnerTurn = db.turnForMessage(message.id);
  assertEquals(spawnerTurn?.status, "interrupted");
});

Deno.test("B3 detached stop path: an explicit interrupt of the spawner cascades to (stops) a runaway detached subagent", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  let subStarted = false;
  const llm = dispatchLlm({
    "hi": [
      program(`await spawn("bg runaway"); console.log("spawned");`),
      textRound("turn over"),
    ],
    // Never finishes on its own — only the cascaded interrupt can stop it.
    "bg runaway": [abortRound(() => (subStarted = true))],
  });
  const { done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await done; // the spawner turn ends normally; the detached child runs on
  await until(() => subStarted);

  // Interrupting the (now idle) spawner cascades to its detached child.
  assertEquals(interruptTurn(spawner.id), true);
  const sub = db.listSessions().find((s) => s.kind === "subagent")!;
  await until(() => db.turnForMessage(db.messagesFor(sub.id).at(-1)!.id)?.status === "interrupted");
  // (Detached surviving a NORMAL turn end is covered by the spawn() non-blocking
  // test above — only an EXPLICIT interrupt cascades.)
});

Deno.test("B4 timeout: a subagent that overruns BOUGH_SUBAGENT_TIMEOUT_MS is auto-interrupted (ok:false)", async () => {
  const prev = Deno.env.get("BOUGH_SUBAGENT_TIMEOUT_MS");
  Deno.env.set("BOUGH_SUBAGENT_TIMEOUT_MS", "150");
  try {
    const db = new Db(":memory:");
    const bus = new Bus();
    const spawner = seed(db);
    const llm = dispatchLlm({
      "hi": [
        program(`const r = await agent("runaway"); console.log("ok=" + r.ok);`),
        textRound("timed out"),
      ],
      "runaway": [abortRound()], // never finishes on its own; the timeout fires
    });
    const { message, done } = beginTurn(failCtx(db, bus, llm), spawner.id);
    await done;
    const sub = db.listSessions().find((s) => s.kind === "subagent")!;
    assertEquals(db.turnForMessage(db.messagesFor(sub.id).at(-1)!.id)?.status, "interrupted");
    assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "ok=false");
  } finally {
    if (prev === undefined) Deno.env.delete("BOUGH_SUBAGENT_TIMEOUT_MS");
    else Deno.env.set("BOUGH_SUBAGENT_TIMEOUT_MS", prev);
  }
});

Deno.test('C5 bad launch: agent("") rejects with a clear error the model sees (no phantom subagent)', async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(`try { await agent(""); } catch (e) { console.log("caught:" + e.message); }`),
      textRound("ok"),
    ],
  });
  const { message, done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await done;
  assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "caught:");
  assertStringIncludes(lastToolResult(db.getMessage(message.id)!), "non-empty string");
  assertEquals(db.listSessions().some((s) => s.kind === "subagent"), false);
});

Deno.test("C6 fan-out: Promise.allSettled (the guided pattern) keeps a good sibling's result when another launch fails", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db);
  const llm = dispatchLlm({
    "hi": [
      program(
        `const rs = await Promise.allSettled([agent("good sibling"), agent("")]);
         console.log("good=" + rs[0].value.report + " bad=" + rs[1].status);`,
      ),
      textRound("recovered"),
    ],
    "good sibling": [textRound("sibling done")],
  });
  const { message, done } = beginTurn(failCtx(db, bus, llm), spawner.id);
  await done;
  // allSettled preserves BOTH outcomes: the good sibling's report is obtained,
  // and the bad launch surfaces as a rejection — no result is discarded. (Raw
  // Promise.all would fail-fast and strand the sibling, which is why the
  // delegation prompt now recommends allSettled.)
  const out = lastToolResult(db.getMessage(message.id)!);
  assertStringIncludes(out, "good=sibling done");
  assertStringIncludes(out, "bad=rejected");
});

import { recoverOrphanedTurns, startTurn } from "./supervisor/turns.ts";

Deno.test("D1 orphan recovery: a subagent stranded by a restart surfaces in the SPAWNER's thread (not silently stuck)", () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const spawner = seed(db); // root "s1"
  // A subagent mid-turn: kind=subagent, origin=spawner, a running turn + pending msg.
  const subId = "sub-orphan";
  db.createSession({
    id: subId,
    parentId: null,
    title: "risky background job",
    kind: "subagent",
    createdAt: 3,
    originId: spawner.id,
    originMessageId: "m0",
  } as unknown as Session);
  const pending: Message = {
    id: "sm1",
    sessionId: subId,
    role: "supervisor",
    parts: [],
    pending: true,
    createdAt: 4,
  };
  db.createMessage(pending);
  startTurn(db, subId, pending.id); // leaves a "running" turn row

  const recovered = recoverOrphanedTurns(db, bus);
  assertEquals(recovered, 1);
  // The subagent's own message is finished with the restart notice...
  assertEquals(db.getMessage(pending.id)!.pending, false);
  // ...and the SPAWNER learns about it — a system note lands in its thread.
  const note = db.messagesFor(spawner.id).find((m) => m.role === "system");
  assertExists(note);
  const text = (note!.parts[0] as { text: string }).text;
  assertStringIncludes(text, "[subagent finished]");
  assertStringIncludes(text, "ORPHANED");
  assertStringIncludes(text, subId);
});

// ---- spawn-time title stub -------------------------------------------------

Deno.test("taskStubTitle: short task passes through, long task word-truncates to ~40 chars", () => {
  assertEquals(taskStubTitle("Fix the login bug"), "Fix the login bug");
  // First line only, whitespace collapsed.
  assertEquals(taskStubTitle("  Fix   the\tlogin bug\nwith lots of detail"), "Fix the login bug");
  // Word-truncated: cut lands at the last word boundary inside 40 chars.
  const stub = taskStubTitle(
    "Refactor the authentication middleware to support refresh tokens",
  );
  assertEquals(stub, "Refactor the authentication middleware…");
  // A single unbroken word keeps the hard cut instead of collapsing to nothing.
  assertEquals(taskStubTitle("x".repeat(60)), "x".repeat(40) + "…");
  // Empty/whitespace tasks fall back to the untitled placeholder.
  assertEquals(taskStubTitle("   \n  "), "untitled");
});

// ---- report is never empty -------------------------------------------------

Deno.test("buildResult never returns an empty report: guard text when present, status-derived fallback otherwise", async () => {
  const db = new Db(":memory:");

  const mk = (status: TurnStatus | null, parts: Part[]) => {
    const sid = `sub-${crypto.randomUUID().slice(0, 8)}`;
    db.createSession({ id: sid, parentId: null, title: "sub", kind: "subagent", createdAt: 1 });
    const mid = `m-${crypto.randomUUID().slice(0, 8)}`;
    db.createMessage({
      id: mid,
      sessionId: sid,
      role: "supervisor",
      parts,
      pending: false,
      createdAt: 2,
    });
    if (status) {
      db.createTurn({
        id: `t-${crypto.randomUUID().slice(0, 8)}`,
        sessionId: sid,
        messageId: mid,
        status,
        step: "done",
        updatedAt: 3,
        firstOutputAt: null,
      });
    }
    return buildResult(db, sid, "sub", mid, undefined);
  };

  // (a) Guard path: a normally-completing turn always wrote a text answer — the
  // turn runner's mute guard (saidSomething → REPORT_NUDGE → forceText, proven in
  // turn.test.ts) runs for subagents too (they go through the same beginTurn/drive).
  // buildResult surfaces that text verbatim: a non-empty report, NOT the fallback.
  const done = await mk("done", [{ type: "text", text: "fixed the parser; check passed" }]);
  assertEquals(done.report, "fixed the parser; check passed");
  assertEquals(done.ok, true);

  // (b) Fallbacks: a turn that ended with no text part still hands back a concise,
  // non-empty, status-derived report — never empty/whitespace (belt-and-suspenders
  // for done/error/interrupt, the real path for an orphaned turn).
  assertEquals((await mk("done", [])).report, "Subagent finished without a written report.");
  assertEquals((await mk("error", [])).report, "Subagent errored before reporting.");
  assertEquals(
    (await mk("interrupted", [])).report,
    "Subagent was interrupted before reporting.",
  );
  // No turn row at all reads as orphaned (a server restart lost the running turn).
  const orphaned = await mk(null, []);
  assertEquals(orphaned.status, "orphaned");
  assertEquals(orphaned.report, "Subagent was orphaned (e.g. server restart) before reporting.");

  // Whitespace-only text counts as empty and falls back too (report is .trim()'d).
  assertEquals(
    (await mk("done", [{ type: "text", text: "   \n  " }])).report,
    "Subagent finished without a written report.",
  );
});

Deno.test("cleanSubagentName: trims and flattens, or falls through to the stub", () => {
  assertEquals(cleanSubagentName("audit seatbelt profile"), "audit seatbelt profile");
  assertEquals(cleanSubagentName("  port   mitmproxy\naddon "), "port mitmproxy addon");
  // Absent or blank -> undefined, so launch() falls back to the task stub.
  assertEquals(cleanSubagentName(undefined), undefined);
  assertEquals(cleanSubagentName(""), undefined);
  assertEquals(cleanSubagentName("   \n\t "), undefined);
  // Rendered straight into the rail/cards/picker, so control bytes never survive.
  assertEquals(cleanSubagentName("bad\u001b[31mname"), "bad [31mname");
  // Long names are cut rather than allowed to blow out the rail's row.
  const long = cleanSubagentName("x".repeat(80))!;
  assertEquals(long.length, 48);
  assertEquals(long.endsWith("\u2026"), true);
  // A non-string is a caller bug worth an error, not a silent coercion.
  assertThrows(() => cleanSubagentName(42), Error, "must be a string");
});
