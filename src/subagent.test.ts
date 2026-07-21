/**
 * Subagent integration: a supervisor program calls agent() (through run_steps and
 * the sealed VM), a real subagent session spins up as a tree branch, works its own
 * shadow worktree, and the spawner adopts its changes. The LLM is a dispatcher keyed
 * by the thread's first user text, because spawner and subagent turns interleave
 * on the same injected client.
 */
import { assert, assertEquals, assertExists, assertStringIncludes } from "jsr:@std/assert@1";
import { Db } from "./db/db.ts";
import { Bus } from "./bus.ts";
import type { Message, Session } from "./schema/parts.ts";
import type { LlmClient, LlmParams, LlmResult } from "./supervisor/llm.ts";
import { defaultTools } from "./tools/mod.ts";
import { beginTurn, interruptTurn, startUserTurn, type TurnCtx } from "./turn.ts";
import { taskStubTitle } from "./subagent.ts";
import * as shadow from "./vcs/shadow.ts";
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

Deno.test({
  name: "repo workspace: subagent works an isolated shadow branch; adopt() folds it back",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subBase = await Deno.makeTempDir({ prefix: "subagent-ws-" });
    const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
    const shadowBase2 = await Deno.makeTempDir({ prefix: "subagent-shadow-" });
    const prevSub = Deno.env.get("BOUGH_SUBAGENT_BASE");
    const prevSnap = Deno.env.get("BOUGH_SNAPSHOT_BASE");
    const prevJj = Deno.env.get("BOUGH_SHADOW_BASE");
    Deno.env.set("BOUGH_SUBAGENT_BASE", subBase);
    Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
    Deno.env.set("BOUGH_SHADOW_BASE", shadowBase2);
    const db = new Db(":memory:");
    const bus = new Bus();
    try {
      const spawner = seed(db, repo);
      const llm = dispatchLlm({
        "hi": [
          program(
            `const r = await agent("make sub.txt with the text from-sub");
             console.log("changed:" + JSON.stringify(r.changedFiles) + " check:" + r.checkPassed);
             console.log(await adopt(r.sessionId));`,
          ),
          textRound("adopted"),
        ],
        "make sub.txt": [
          program(`await write("sub.txt", "from-sub\\n"); console.log("wrote");`, { done: true }),
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

      const final = db.getMessage(message.id)!;
      const out = lastToolResult(final);
      // The subagent's edit happened on its own branch and was reported…
      assertStringIncludes(out, 'changed:["sub.txt"]');
      // …with done accepted (no check declared → accepted).
      assertStringIncludes(out, "check:true");
      assertStringIncludes(out, "adopted");

      // The subagent ran in its own workspace dir, not the spawner's checkout.
      const sub = db.listSessions().find((s) => s.kind === "subagent")!;
      const subDir = db.getSessionRuntime(sub.id).workspace!;
      assert(subDir.startsWith(subBase), `subagent dir ${subDir} outside ${subBase}`);
      assertEquals(await Deno.readTextFile(`${subDir}/sub.txt`), "from-sub\n");

      // Adoption landed the file in the spawner's own worktree and session tip.
      // External mode: the spawner itself was relocated off the repo checkout, so
      // the user's repo stays pristine — no adopted files.
      const spawnerDir = db.getSessionRuntime(spawner.id).workspace!;
      assert(spawnerDir !== repo, "spawner should run in its own working copy");
      assertEquals(await Deno.readTextFile(`${spawnerDir}/sub.txt`), "from-sub\n");
      const spawnerDiff = await shadow.diff(spawnerDir, spawner.id);
      assert(spawnerDiff.files.some((f) => f.path === "sub.txt"));
      for (const leaked of [".jj", "sub.txt"]) {
        let inRepo = true;
        try {
          await Deno.stat(`${repo}/${leaked}`);
        } catch {
          inRepo = false;
        }
        assertEquals(inRepo, false, `${leaked} leaked into the repo checkout`);
      }
      // The subagent branch emptied but survives — continuable, not consumed.
      assertEquals((await shadow.diff(subDir, sub.id)).files.length, 0);
    } finally {
      if (prevSub === undefined) Deno.env.delete("BOUGH_SUBAGENT_BASE");
      else Deno.env.set("BOUGH_SUBAGENT_BASE", prevSub);
      if (prevSnap === undefined) Deno.env.delete("BOUGH_SNAPSHOT_BASE");
      else Deno.env.set("BOUGH_SNAPSHOT_BASE", prevSnap);
      if (prevJj === undefined) Deno.env.delete("BOUGH_SHADOW_BASE");
      else Deno.env.set("BOUGH_SHADOW_BASE", prevJj);
      await Deno.remove(repo, { recursive: true }).catch(() => {});
      await Deno.remove(subBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
      await Deno.remove(shadowBase2, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // The nested adopt chain: a grandchild must
  // get its own branched dir, not run on the subagent's working copy.
  name: "nested repo delegation: grandchild works its own branch; adopts chain upward",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subBase = await Deno.makeTempDir({ prefix: "subagent-ws-" });
    const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
    const shadowBase2 = await Deno.makeTempDir({ prefix: "subagent-shadow-" });
    const prevSub = Deno.env.get("BOUGH_SUBAGENT_BASE");
    const prevSnap = Deno.env.get("BOUGH_SNAPSHOT_BASE");
    const prevJj = Deno.env.get("BOUGH_SHADOW_BASE");
    Deno.env.set("BOUGH_SUBAGENT_BASE", subBase);
    Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
    Deno.env.set("BOUGH_SHADOW_BASE", shadowBase2);
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
          textRound("all adopted"),
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
      // The grandchild's work rode the adopt chain: grandchild → subagent → spawner
      // (whose working copy is its own relocated dir, not the repo checkout).
      assertStringIncludes(out, 'subchanged:["nested.txt"]');
      assertStringIncludes(out, "adopted");
      const spawnerDir = db.getSessionRuntime(spawner.id).workspace!;
      assertEquals(await Deno.readTextFile(`${spawnerDir}/nested.txt`), "from-nested\n");

      // Three tiers of sessions; the grandchild's lineage points at the subagent.
      const subs = db.listSessions().filter((s) => s.kind === "subagent");
      assertEquals(subs.length, 2);
      const sub = subs.find((s) => s.originId === spawner.id)!;
      const grandchild = subs.find((s) => s.originId === sub.id)!;
      assertExists(grandchild);

      // The grandchild ran in its OWN branched dir, distinct from the subagent's.
      const subDir = db.getSessionRuntime(sub.id).workspace!;
      const grandDir = db.getSessionRuntime(grandchild.id).workspace!;
      assert(grandDir.startsWith(subBase), `grandchild dir ${grandDir} outside ${subBase}`);
      assert(grandDir !== subDir, "grandchild shared the subagent's working copy");
      // Both branches emptied by their adoptions but survive — still continuable.
      assertEquals((await shadow.diff(grandDir, grandchild.id)).files.length, 0);
      assertEquals((await shadow.diff(subDir, sub.id)).files.length, 0);
    } finally {
      if (prevSub === undefined) Deno.env.delete("BOUGH_SUBAGENT_BASE");
      else Deno.env.set("BOUGH_SUBAGENT_BASE", prevSub);
      if (prevSnap === undefined) Deno.env.delete("BOUGH_SNAPSHOT_BASE");
      else Deno.env.set("BOUGH_SNAPSHOT_BASE", prevSnap);
      if (prevJj === undefined) Deno.env.delete("BOUGH_SHADOW_BASE");
      else Deno.env.set("BOUGH_SHADOW_BASE", prevJj);
      await Deno.remove(repo, { recursive: true }).catch(() => {});
      await Deno.remove(subBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
      await Deno.remove(shadowBase2, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // "Continue off of it": a subagent is a plain session — a human message posted to
  // it (the same startUserTurn path behind POST /sessions/:id/messages) runs a new
  // turn in ITS jj workspace, stacking edits on its branch, spawner untouched.
  name: "a finished subagent accepts human messages and keeps working its own branch",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subBase = await Deno.makeTempDir({ prefix: "subagent-ws-" });
    const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
    const shadowBase2 = await Deno.makeTempDir({ prefix: "subagent-shadow-" });
    const prevSub = Deno.env.get("BOUGH_SUBAGENT_BASE");
    const prevSnap = Deno.env.get("BOUGH_SNAPSHOT_BASE");
    const prevJj = Deno.env.get("BOUGH_SHADOW_BASE");
    Deno.env.set("BOUGH_SUBAGENT_BASE", subBase);
    Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
    Deno.env.set("BOUGH_SHADOW_BASE", shadowBase2);
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
      // …and its edit stacked onto the subagent's branch, not the spawner's.
      const subDir = db.getSessionRuntime(sub.id).workspace!;
      const files = (await shadow.diff(subDir, sub.id)).files.map((f) => f.path).sort();
      assertEquals(files, ["more.txt", "sub.txt"]);
      let leaked = true;
      try {
        await Deno.stat(`${repo}/more.txt`);
      } catch {
        leaked = false;
      }
      assertEquals(leaked, false, "follow-up edit leaked into the spawner checkout");
    } finally {
      if (prevSub === undefined) Deno.env.delete("BOUGH_SUBAGENT_BASE");
      else Deno.env.set("BOUGH_SUBAGENT_BASE", prevSub);
      if (prevSnap === undefined) Deno.env.delete("BOUGH_SNAPSHOT_BASE");
      else Deno.env.set("BOUGH_SNAPSHOT_BASE", prevSnap);
      if (prevJj === undefined) Deno.env.delete("BOUGH_SHADOW_BASE");
      else Deno.env.set("BOUGH_SHADOW_BASE", prevJj);
      await Deno.remove(repo, { recursive: true }).catch(() => {});
      await Deno.remove(subBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
      await Deno.remove(shadowBase2, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  name: "parallel subagents: two agent() calls in Promise.all work disjoint branches",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subBase = await Deno.makeTempDir({ prefix: "subagent-ws-" });
    const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
    const shadowBase2 = await Deno.makeTempDir({ prefix: "subagent-shadow-" });
    const prevSub = Deno.env.get("BOUGH_SUBAGENT_BASE");
    const prevSnap = Deno.env.get("BOUGH_SNAPSHOT_BASE");
    const prevJj = Deno.env.get("BOUGH_SHADOW_BASE");
    Deno.env.set("BOUGH_SUBAGENT_BASE", subBase);
    Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
    Deno.env.set("BOUGH_SHADOW_BASE", shadowBase2);
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
      assertStringIncludes(out, 'a:["a.txt"]');
      assertStringIncludes(out, 'b:["b.txt"]');

      // Two subagent lanes, each on its own branch; neither edit leaked into the
      // spawner's checkout (nothing was adopted).
      const subs = db.listSessions().filter((s) => s.kind === "subagent");
      assertEquals(subs.length, 2);
      const dirs = subs.map((s) => db.getSessionRuntime(s.id).workspace!);
      assert(dirs[0] !== dirs[1]);
      for (const f of ["a.txt", "b.txt"]) {
        let inSpawner = true;
        try {
          await Deno.stat(`${repo}/${f}`);
        } catch {
          inSpawner = false;
        }
        assertEquals(inSpawner, false, `${f} leaked into the spawner checkout`);
      }
    } finally {
      if (prevSub === undefined) Deno.env.delete("BOUGH_SUBAGENT_BASE");
      else Deno.env.set("BOUGH_SUBAGENT_BASE", prevSub);
      if (prevSnap === undefined) Deno.env.delete("BOUGH_SNAPSHOT_BASE");
      else Deno.env.set("BOUGH_SNAPSHOT_BASE", prevSnap);
      if (prevJj === undefined) Deno.env.delete("BOUGH_SHADOW_BASE");
      else Deno.env.set("BOUGH_SHADOW_BASE", prevJj);
      await Deno.remove(repo, { recursive: true }).catch(() => {});
      await Deno.remove(subBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
      await Deno.remove(shadowBase2, { recursive: true }).catch(() => {});
    }
  },
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
            { done: true },
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
