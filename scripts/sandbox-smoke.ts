/**
 * Manual end-to-end smoke for the sandboxed turn. NOT part of `deno task test`.
 * Requires git and macOS (sandbox-exec). No API key — the LLM is scripted.
 *
 *   deno run --allow-net --allow-env --allow-read --allow-write --allow-run --allow-ffi --allow-sys \
 *     scripts/sandbox-smoke.ts
 *
 * Drives a real turn in a throwaway git repo and asserts:
 *   1. write_file inside the workspace succeeds,
 *   2. bash writing OUTSIDE the workspace is denied by Seatbelt,
 *   3. the shadow diff shows the edits the session made.
 */
import { Db } from "../src/db/db.ts";
import { Bus } from "../src/bus.ts";
import { beginTurn } from "../src/turn.ts";
import * as shadow from "../src/vcs/shadow.ts";
import type { LlmClient, LlmParams, LlmResult } from "../src/supervisor/llm.ts";

if (Deno.build.os !== "darwin") {
  console.error("sandbox smoke is macOS-only (sandbox-exec).");
  Deno.exit(2);
}

async function sh(cmd: string, cwd: string): Promise<void> {
  const r = await new Deno.Command("sh", { args: ["-c", cmd], cwd, stdout: "null", stderr: "inherit" }).output();
  if (r.code !== 0) throw new Error(`setup command failed: ${cmd}`);
}

function scriptedLlm(rounds: LlmResult[]): LlmClient {
  let i = 0;
  return { run: (_p: LlmParams) => Promise.resolve(rounds[i++]) };
}

const repo = await Deno.makeTempDir({ prefix: "bough-smoke-" });
const escape = `${Deno.env.get("HOME")}/bough-smoke-escape-${crypto.randomUUID()}.txt`;
const snapBase = await Deno.makeTempDir({ prefix: "bough-snap-" });
const shadowBase = await Deno.makeTempDir({ prefix: "bough-shadow-" });
const wsBase = await Deno.makeTempDir({ prefix: "bough-ws-" });
Deno.env.set("BOUGH_SNAPSHOT_BASE", snapBase);
Deno.env.set("BOUGH_SHADOW_BASE", shadowBase);
Deno.env.set("BOUGH_SUBAGENT_BASE", wsBase);

try {
  // A real git repo with one committed file.
  await sh("git init -q && git config user.email a@b.c && git config user.name t", repo);
  await Deno.writeTextFile(`${repo}/tracked.txt`, "original\n");
  await sh("git add -A && git commit -qm init", repo);

  const db = new Db(":memory:");
  const bus = new Bus();
  const sessionId = "smoke";
  db.createSession({ id: sessionId, parentId: null, title: "smoke", kind: "root", createdAt: Date.now() });
  db.setSessionWorkspace(sessionId, repo);
  db.createMessage({
    id: "u",
    sessionId,
    role: "user",
    parts: [{ type: "text", text: "go" }],
    pending: false,
    createdAt: Date.now(),
  });

  // Round 1: three tool calls (write inside, bash escape attempt, edit tracked). Round 2: done.
  const llm = scriptedLlm([
    {
      stopReason: "tool_use",
      content: [
        { type: "tool_use", id: "w", name: "write_file", input: { path: "created.txt", content: "made in sandbox\n" } },
        { type: "tool_use", id: "b", name: "bash", input: { command: `echo pwn > '${escape}'` } },
        { type: "tool_use", id: "e", name: "edit_file", input: { path: "tracked.txt", old_string: "original", new_string: "edited" } },
      ],
    },
    { stopReason: "end_turn", content: [{ type: "text", text: "finished" }] },
  ]);

  const { message, done } = beginTurn({ db, bus, llm }, sessionId);
  await done;

  const parts = db.getMessage(message.id)!.parts;
  const fail = (m: string) => {
    console.error("FAIL:", m);
    console.error(JSON.stringify(parts, null, 2));
    Deno.exit(1);
  };

  // The turn relocated the session into its shadow worktree; assert there.
  const ws = db.getSessionRuntime(sessionId).workspace!;
  if (ws === repo) fail("session was not relocated into a shadow worktree");

  // 1. write inside succeeded (in the worktree — the origin stays untouched).
  if (!(await Deno.stat(`${ws}/created.txt`).then(() => true).catch(() => false))) fail("created.txt missing");
  if (await Deno.stat(`${repo}/created.txt`).then(() => true).catch(() => false)) fail("created.txt leaked into the origin");

  // 2. escape write denied.
  const leaked = await Deno.stat(escape).then(() => true).catch(() => false);
  if (leaked) fail(`escape write was NOT denied: ${escape}`);
  const bashResult = parts.find((p) => p.type === "tool_result" && p.callId === "b") as { output: string } | undefined;
  console.log("bash escape result:", JSON.stringify(bashResult?.output));

  // 3. the shadow diff shows the session's edits.
  const diff = await shadow.diff(ws, sessionId);
  const files = diff.files.map((f) => f.path).sort();
  console.log("shadow diff files:", files);
  if (!files.includes("created.txt") || !files.includes("tracked.txt")) fail("shadow diff missing expected files");

  // 4. Fork-at-first-turn: a kind=fork session branches off the parent's tip, so its
  //    change-vs-parent diff shows only what the fork itself added.
  const forkId = "smoke-fork";
  db.createSession({ id: forkId, parentId: sessionId, title: "fork", kind: "fork", createdAt: Date.now() });
  db.setSessionWorkspace(forkId, ws); // forks inherit the parent's workspace column
  db.createMessage({
    id: "uf",
    sessionId: forkId,
    role: "user",
    parts: [{ type: "text", text: "go" }],
    pending: false,
    createdAt: Date.now(),
  });
  const forkLlm = scriptedLlm([
    {
      stopReason: "tool_use",
      content: [{ type: "tool_use", id: "fw", name: "write_file", input: { path: "fork-only.txt", content: "fork\n" } }],
    },
    { stopReason: "end_turn", content: [{ type: "text", text: "done" }] },
  ]);
  const forkTurn = beginTurn({ db, bus, llm: forkLlm }, forkId);
  await forkTurn.done;
  const forkWs = db.getSessionRuntime(forkId).workspace!;
  const forkDiff = await shadow.diff(forkWs, forkId);
  const forkFiles = forkDiff.files.map((f) => f.path).sort();
  console.log("fork diff files:", forkFiles);
  if (!forkFiles.includes("fork-only.txt")) {
    console.error("FAIL: fork diff missing fork-only.txt");
    Deno.exit(1);
  }

  console.log("\nOK — write-inside ok, escape denied, shadow diff shows the edits, fork branches off the parent tip");
} finally {
  Deno.env.delete("BOUGH_SNAPSHOT_BASE");
  Deno.env.delete("BOUGH_SHADOW_BASE");
  Deno.env.delete("BOUGH_SUBAGENT_BASE");
  await Deno.remove(repo, { recursive: true }).catch(() => {});
  await Deno.remove(snapBase, { recursive: true }).catch(() => {});
  await Deno.remove(shadowBase, { recursive: true }).catch(() => {});
  await Deno.remove(wsBase, { recursive: true }).catch(() => {});
  await Deno.remove(escape).catch(() => {});
}
