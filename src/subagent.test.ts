/**
 * Subagent integration: a supervisor program calls agent() (through run_steps and
 * the sealed VM), a real subagent session spins up as a tree branch, works its own
 * jj workspace, and the spawner adopts its changes. The LLM is a dispatcher keyed
 * by the thread's first user text, because spawner and subagent turns interleave
 * on the same injected client.
 */
import { assert, assertEquals, assertExists, assertStringIncludes } from "jsr:@std/assert@1";
import { Db } from "./db/db.ts";
import { Bus } from "./bus.ts";
import type { Message, Session } from "./schema/parts.ts";
import type { LlmClient, LlmParams, LlmResult } from "./supervisor/llm.ts";
import { defaultTools } from "./tools/mod.ts";
import { beginTurn, startUserTurn, type TurnCtx } from "./turn.ts";
import * as jj from "./vcs/jj.ts";

// ---- harness ---------------------------------------------------------------

/**
 * Scripted rounds per conversation, keyed by the LAST text-bearing user message
 * (tool_result-only user messages don't carry text) — so a continued thread
 * dispatches on its newest human message, not the original task. A round may be a
 * thunk returning a promise, to gate WHEN that thread's reply lands (background
 * subagent timing).
 */
type ScriptedRound = LlmResult | (() => Promise<LlmResult>);
function dispatchLlm(scripts: Record<string, ScriptedRound[]>): LlmClient {
  const idx: Record<string, number> = {};
  return {
    async run(params: LlmParams, onText: (d: string) => void): Promise<LlmResult> {
      const text = [...params.messages].reverse()
        .filter((m) => m.role === "user")
        .map((m) => (m.content.find((b) => b.type === "text") as { text?: string } | undefined)?.text)
        .find((t) => t !== undefined) ?? "";
      const key = Object.keys(scripts).find((k) => text.startsWith(k));
      if (!key) throw new Error(`no script for thread starting with: ${text.slice(0, 60)}`);
      const i = idx[key] ?? 0;
      idx[key] = i + 1;
      const scripted = scripts[key][i];
      if (!scripted) throw new Error(`script "${key}" exhausted at round ${i + 1}`);
      const result = typeof scripted === "function" ? await scripted() : scripted;
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

const jjAvailable = (await canRun("jj")) && (await canRun("git")) &&
  await (async () => {
    try {
      await jj.version();
      return true;
    } catch {
      return false;
    }
  })();

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
  const ctx: TurnCtx = { db, bus, llm, tools: defaultTools, titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)) };

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
      program(`const h = await spawn("background research"); console.log("spawned:" + (h.sessionId ? "yes" : "no"));`),
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
  const ctx: TurnCtx = { db, bus, llm, tools: defaultTools, titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)) };

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
  const ctx: TurnCtx = { db, bus, llm, tools: defaultTools, titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)) };

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

Deno.test("subagents cannot spawn subagents (no agent() host function at depth 1)", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  db.createSession({
    id: "sub1",
    parentId: null,
    title: "subagent",
    kind: "subagent",
    createdAt: 1,
    originId: "elsewhere",
    originMessageId: "m0",
  });
  db.createMessage({
    id: "u1",
    sessionId: "sub1",
    role: "user",
    parts: [{ type: "text", text: "try to delegate" }],
    pending: false,
    createdAt: 2,
  });
  const llm = dispatchLlm({
    "try to delegate": [
      program(`const r = await agent("nested"); console.log(r);`),
      textRound("could not"),
    ],
  });
  const ctx: TurnCtx = { db, bus, llm, tools: defaultTools, titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)) };

  const { message, done } = beginTurn(ctx, "sub1");
  await done;

  const final = db.getMessage(message.id)!;
  assertStringIncludes(lastToolResult(final), "unknown host function: agent");
  assertEquals(db.listSessions().filter((s) => s.kind === "subagent").length, 1);
});

Deno.test({
  name: "repo workspace: subagent works an isolated jj branch; adopt() squashes it back",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subBase = await Deno.makeTempDir({ prefix: "subagent-ws-" });
    const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
    const prevSub = Deno.env.get("BOUGH_SUBAGENT_BASE");
    const prevSnap = Deno.env.get("BOUGH_SNAPSHOT_BASE");
    Deno.env.set("BOUGH_SUBAGENT_BASE", subBase);
    Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
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
      const ctx: TurnCtx = { db, bus, llm, tools: defaultTools, titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)) };

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

      // Adoption landed the file in the spawner's workspace and its jj change.
      assertEquals(await Deno.readTextFile(`${repo}/sub.txt`), "from-sub\n");
      const spawnerDiff = await jj.diff(repo, spawner.id);
      assert(spawnerDiff.files.some((f) => f.path === "sub.txt"));
      // The subagent branch emptied but survives — continuable, not consumed.
      assertEquals((await jj.diff(subDir, sub.id)).files.length, 0);
    } finally {
      if (prevSub === undefined) Deno.env.delete("BOUGH_SUBAGENT_BASE");
      else Deno.env.set("BOUGH_SUBAGENT_BASE", prevSub);
      if (prevSnap === undefined) Deno.env.delete("BOUGH_SNAPSHOT_BASE");
      else Deno.env.set("BOUGH_SNAPSHOT_BASE", prevSnap);
      await Deno.remove(repo, { recursive: true }).catch(() => {});
      await Deno.remove(subBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // "Continue off of it": a subagent is a plain session — a human message posted to
  // it (the same startUserTurn path behind POST /sessions/:id/messages) runs a new
  // turn in ITS jj workspace, stacking edits on its branch, spawner untouched.
  name: "a finished subagent accepts human messages and keeps working its own branch",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subBase = await Deno.makeTempDir({ prefix: "subagent-ws-" });
    const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
    const prevSub = Deno.env.get("BOUGH_SUBAGENT_BASE");
    const prevSnap = Deno.env.get("BOUGH_SNAPSHOT_BASE");
    Deno.env.set("BOUGH_SUBAGENT_BASE", subBase);
    Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
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
      const ctx: TurnCtx = { db, bus, llm, tools: defaultTools, titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)) };

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
      const files = (await jj.diff(subDir, sub.id)).files.map((f) => f.path).sort();
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
      await Deno.remove(repo, { recursive: true }).catch(() => {});
      await Deno.remove(subBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  name: "parallel subagents: two agent() calls in Promise.all work disjoint branches",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subBase = await Deno.makeTempDir({ prefix: "subagent-ws-" });
    const snapBase = await Deno.makeTempDir({ prefix: "subagent-snap-" });
    const prevSub = Deno.env.get("BOUGH_SUBAGENT_BASE");
    const prevSnap = Deno.env.get("BOUGH_SNAPSHOT_BASE");
    Deno.env.set("BOUGH_SUBAGENT_BASE", subBase);
    Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
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
      const ctx: TurnCtx = { db, bus, llm, tools: defaultTools, titler: (t) => Promise.resolve("titled: " + t.slice(0, 20)) };

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
      await Deno.remove(repo, { recursive: true }).catch(() => {});
      await Deno.remove(subBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
    }
  },
});
