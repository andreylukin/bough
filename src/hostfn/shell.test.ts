/**
 * Shell verb tests (T3.2).
 *
 * These run REAL subprocesses, which is the only way to test the thing that
 * matters: the auto-background handoff is a race between a live child and a
 * threshold, and a fake process cannot lose that race the way a real one does.
 * They are still hermetic — no network, no API key, no `~/.bough` — because every
 * command is a `/bin/sh` builtin or a temp-directory file, and every registry is
 * constructed per test rather than shared.
 *
 * The threshold and the `sh` deadline are injected (`ShellOptions`) so the handoff
 * is exercised in milliseconds. A test that had to set `BOUGH_BASH_BG_AFTER_MS` and
 * wait 60s would be neither hermetic nor parallel-safe.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json`
 * cannot resolve. (Same constraint `hostfn/patch.test.ts` and `bus.test.ts`
 * document.)
 */

import { test } from "bun:test";
import { deepStrictEqual, ok } from "node:assert";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { ConflictError, NotFoundError, ProgramError } from "../errors.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { BackgroundJob } from "../schema/parts.ts";
import {
  descendantPids,
  JobRegistry,
  MAX_HEAD_CHARS,
  MAX_TAIL_CHARS,
  shellText,
  truncateMiddle,
} from "./jobs.ts";
import { bash, createShellHostFns, shConcurrent, type ShellCtx } from "./shell.ts";

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

/** `deepStrictEqual`, wrapped: aliasing it directly loses its assertion signature. */
function eq(actual: unknown, expected: unknown, message?: string): void {
  deepStrictEqual(actual, expected, message);
}

/** Substring assertion that prints the whole haystack on failure. */
function has(haystack: string, needle: string): void {
  ok(
    haystack.includes(needle),
    `expected to contain ${JSON.stringify(needle)}, got:\n${haystack}`,
  );
}

function lacks(haystack: string, needle: string, why: string): void {
  ok(!haystack.includes(needle), `${why} — but found ${JSON.stringify(needle)} in:\n${haystack}`);
}

function matches(text: string, re: RegExp): void {
  ok(re.test(text), `expected ${re} to match:\n${text}`);
}

// deno-lint-ignore no-explicit-any
type Ctor<T> = new (...args: any[]) => T;

/** Run `fn` and return the error it threw, asserting its type. */
function throwsWith<T extends Error>(fn: () => unknown, ctor: Ctor<T>): T {
  try {
    fn();
  } catch (err) {
    ok(err instanceof ctor, `expected ${ctor.name}, got ${err}`);
    return err as T;
  }
  throw new Error(`expected ${ctor.name}, but nothing was thrown`);
}

/** Await `fn` and return the error it rejected with, asserting its type. */
async function rejectsWith<T extends Error>(
  fn: () => Promise<unknown>,
  ctor: Ctor<T>,
): Promise<T> {
  try {
    await fn();
  } catch (err) {
    ok(err instanceof ctor, `expected ${ctor.name}, got ${err}`);
    return err as T;
  }
  throw new Error(`expected ${ctor.name}, but the promise resolved`);
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

interface Rig {
  registry: JobRegistry;
  ctx: ShellCtx;
  events: BoughEvent[];
  notes: { sessionId: string; text: string }[];
  /** SIGTERM anything still running and wait for it, so no test leaks a process. */
  cleanup: () => Promise<void>;
}

function rig(options: { sessionId?: string; signal?: AbortSignal } = {}): Rig {
  const events: BoughEvent[] = [];
  const notes: { sessionId: string; text: string }[] = [];
  const bus = new Bus();
  bus.subscribe((e) => events.push(e));
  const registry = new JobRegistry({
    bus,
    notify: (sessionId, text) => notes.push({ sessionId, text }),
  });
  const sessionId = options.sessionId ?? "sess-1";
  const ctx: ShellCtx = { sessionId, workspace: process.cwd(), signal: options.signal };
  return {
    registry,
    ctx,
    events,
    notes,
    cleanup: async () => {
      registry.killAll();
      await registry.drain();
    },
  };
}

/** Poll `pred` until it holds, or fail after `ms`. */
/** Whether `pid` still exists. Signal 0 tests for existence without delivering one. */
function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}


async function untilTrue(what: string, pred: () => boolean, ms = 10_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (pred()) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`timed out waiting for ${what}`);
}

/**
 * Poll a CURSOR read (`bashOutput`) until the text seen across calls satisfies
 * `check`, and return everything seen. Accumulating is the point: each call
 * consumes what it returns, so a non-accumulating poll would lose the first chunk.
 */
async function untilAccrued(
  what: string,
  read: () => string,
  check: (seen: string) => boolean,
  ms = 10_000,
): Promise<string> {
  const deadline = Date.now() + ms;
  let seen = "";
  while (Date.now() < deadline) {
    seen += read();
    if (check(seen)) return seen;
    await new Promise((r) => setTimeout(r, 20));
  }
  throw new Error(`timed out waiting for ${what}; saw:\n${seen}`);
}

/** Print 200 numbered lines without depending on `seq`. */
const MANY_LINES = "i=1; while [ $i -le 200 ]; do printf 'line%s\\n' $i; i=$((i+1)); done";

// ---------------------------------------------------------------------------
// bash — the auto-background handoff (the headline AC)
// ---------------------------------------------------------------------------

test("bash auto-backgrounds a long command and the job stays readable", async () => {
  const r = rig();
  // Prints once before the threshold, once well after, then holds itself open.
  const out = await bash(
    "printf 'before\\n'; sleep 1.5; printf 'after\\n'; sleep 60",
    r.ctx,
    { registry: r.registry, bgAfterMs: 150 },
  );

  // The handoff note: the id, that it KEEPS RUNNING, and the three verbs by name.
  matches(out, /moved to background as bg_1/);
  has(out, "It keeps running");
  has(out, 'bashOutput("bg_1")');
  has(out, 'bashWait("bg_1")');
  has(out, 'bashKill("bg_1")');
  // Output produced before the handoff rides along rather than being lost.
  has(out, "before");

  // The command really is still running, and its later output is readable.
  const seen = await untilAccrued(
    "post-handoff output",
    () => r.registry.bashOutput("bg_1", r.ctx.sessionId),
    (s) => s.includes("after"),
  );
  has(seen, "[running]");
  lacks(seen, "before", "the cursor must not hand the same output out twice");

  const jobs = r.registry.listJobs(r.ctx.sessionId);
  eq(jobs.length, 1);
  eq(jobs[0].status, "running");
  eq(jobs[0].id, "bg_1");
  eq(r.events.map((e) => e.type), ["job.spawned"]);

  matches(await r.registry.bashKill("bg_1", r.ctx.sessionId), /^killed bg_1 \(/);
  eq(r.events.map((e) => e.type), ["job.spawned", "job.exited"]);
  await r.cleanup();
});

test("an auto-backgrounded command is never killed by the threshold", async () => {
  const r = rig();
  await bash("sleep 60", r.ctx, { registry: r.registry, bgAfterMs: 100 });
  // Well past the threshold, the process is still alive.
  await new Promise((res) => setTimeout(res, 400));
  eq(r.registry.runningIds(r.ctx.sessionId), ["bg_1"]);
  has(r.registry.bashOutput("bg_1", r.ctx.sessionId), "[running]");
  await r.cleanup();
});

test("auto-background ignores the concurrency cap so no command is ever lost", async () => {
  // The cap brakes bashBg loops. A foreground command that merely took a while must
  // still be handed over rather than blocked-then-killed (plan §6.7).
  const r = rig();
  for (let i = 0; i < 8; i++) r.registry.bashBg("sleep 60", r.ctx);
  eq(r.registry.runningIds(r.ctx.sessionId).length, 8);

  const out = await bash("sleep 60", r.ctx, { registry: r.registry, bgAfterMs: 100 });
  matches(out, /moved to background as bg_9/);
  eq(r.registry.runningIds(r.ctx.sessionId).length, 9);
  await r.cleanup();
});

test("bash returns output inline when the command finishes first", async () => {
  const r = rig();
  eq(await bash("printf 'hi\\n'", r.ctx, { registry: r.registry, bgAfterMs: 5_000 }), "hi");
  eq(r.registry.listJobs(r.ctx.sessionId), []);
  eq(r.events, []);
  await r.cleanup();
});

test("bash reports a non-zero exit as data, not as a throw", async () => {
  const r = rig();
  const out = await bash("printf 'nope\\n'; exit 3", r.ctx, {
    registry: r.registry,
    bgAfterMs: 5_000,
  });
  eq(out, "nope\n[exit code 3]");
  eq(await bash("exit 0", r.ctx, { registry: r.registry }), "(no output)");
  await r.cleanup();
});

// ---------------------------------------------------------------------------
// bash — the turn's interrupt
// ---------------------------------------------------------------------------

test("bash on an already-interrupted turn fails without spawning anything", async () => {
  const ac = new AbortController();
  ac.abort();
  const r = rig({ signal: ac.signal });
  const err = await rejectsWith(
    () => bash("printf hi", r.ctx, { registry: r.registry }),
    ProgramError,
  );
  // Spec §6: name WHICH stop happened, and what survived it.
  has(err.message, "the turn was interrupted");
  has(err.message, "still stands");
  eq(r.registry.listJobs(r.ctx.sessionId), []);
  await r.cleanup();
});

test("interrupting a running bash kills the child and keeps its partial output", async () => {
  const ac = new AbortController();
  const r = rig({ signal: ac.signal });
  const running = bash("printf 'partial\\n'; sleep 60", r.ctx, {
    registry: r.registry,
    bgAfterMs: 30_000,
  });
  await untilTrue(
    "in-flight foreground output",
    () => (r.registry.inflightForegroundOutput(r.ctx.sessionId) ?? "").includes("partial"),
  );
  // What the turn runner attaches to the interrupted tool record.
  const partial = r.registry.inflightForegroundOutput(r.ctx.sessionId)!;
  has(partial, "[interrupted] bash");
  has(partial, "partial");

  ac.abort();
  has((await rejectsWith(() => running, ProgramError)).message, "the turn was interrupted");
  // The foreground set empties once the call returns.
  eq(r.registry.inflightForegroundOutput(r.ctx.sessionId), null);
  await r.cleanup();
});

/**
 * THE ONE THAT MATTERS: the interrupt reaches the GRANDCHILD, not just `sh`.
 *
 * The test above asserts the promise rejects, which it did even while the work kept
 * running: `sh -c 'sleep 60'` does not forward SIGTERM, so killing the shell alone
 * reparented `sleep` onto init and it ran to completion — while the TUI printed
 * "interrupting — the program's children are killed". A rejected promise is not a
 * dead process, so this one asserts on `ps`.
 *
 * It fails whenever `JobRegistry.spawn` hands the abort signal to `Bun.spawn`:
 * Bun registers its own listener AT SPAWN TIME, before `killTreeOnAbort` can add
 * ours, so Bun kills the shell first and our tree walk finds an empty tree.
 */
test("interrupting a bash kills the grandchild too, not just the shell", async () => {
  const ac = new AbortController();
  const r = rig({ signal: ac.signal });
  // Diffed against a before-snapshot so only THIS command's processes are asserted
  // on — the suite shares one process and other shells may be in flight.
  const before = new Set(descendantPids(process.pid));
  const running = bash("sleep 47; echo never", r.ctx, { registry: r.registry, bgAfterMs: 30_000 });
  // Two deep: `sh -c` and the `sleep` it does not forward signals to.
  await untilTrue(
    "the shell and its sleep to appear",
    () => descendantPids(process.pid).filter((p) => !before.has(p)).length >= 2,
  );
  const spawned = descendantPids(process.pid).filter((p) => !before.has(p));

  ac.abort();
  has((await rejectsWith(() => running, ProgramError)).message, "the turn was interrupted");
  await untilTrue(
    `every pid of the interrupted command (${spawned.join(",")}) to die`,
    () => spawned.every((pid) => !alive(pid)),
    5_000,
  );
  await r.cleanup();
});

// ---------------------------------------------------------------------------
// sh
// ---------------------------------------------------------------------------

test("sh never throws on a non-zero exit and returns codes in input order", async () => {
  const r = rig();
  const res = await shConcurrent(
    ["exit 3", "printf 'ok\\n'", "exit 1", "printf 'err\\n' >&2; exit 7"],
    r.ctx,
    { registry: r.registry },
  );
  eq(res, [
    { code: 3, out: "" },
    { code: 0, out: "ok" },
    { code: 1, out: "" },
    { code: 7, out: "err" },
  ]);
  await r.cleanup();
});

test("sh reports a command that does not exist rather than throwing", async () => {
  const r = rig();
  const res = await shConcurrent(["definitely-not-a-command-xyzzy"], r.ctx, {
    registry: r.registry,
  });
  eq(res.length, 1);
  ok(res[0].code !== 0, "a missing command must report a non-zero code");
  ok(res[0].out.length > 0, "the shell's own diagnostic must reach the caller");
  await r.cleanup();
});

test("sh runs its commands concurrently", async () => {
  // A rendezvous rather than a stopwatch: each command creates its own marker and
  // then blocks until the other's exists. Both can only finish if they overlap; if
  // they were serialized the first would spin until the deadline killed it.
  const dir = await mkdtemp(join(tmpdir(), "bough-sh-"));
  const r = rig();
  try {
    const meet = (mine: string, theirs: string) =>
      `touch ${dir}/${mine}; while [ ! -f ${dir}/${theirs} ]; do sleep 0.02; done; ` +
      `printf '${mine}\\n'`;
    const res = await shConcurrent([meet("a", "b"), meet("b", "a")], r.ctx, {
      registry: r.registry,
      shTimeoutMs: 10_000,
    });
    eq(res, [{ code: 0, out: "a" }, { code: 0, out: "b" }]);
  } finally {
    await r.cleanup();
    await rm(dir, { recursive: true, force: true });
  }
});

test("sh kills a command that outlives its deadline and names the escape hatch", async () => {
  const r = rig();
  const res = await shConcurrent(["printf 'started\\n'; sleep 60"], r.ctx, {
    registry: r.registry,
    shTimeoutMs: 200,
  });
  eq(res.length, 1);
  has(res[0].out, "killed after 0.2s");
  has(res[0].out, "bashBg()");
  has(res[0].out, "started");
  await r.cleanup();
});

// ---------------------------------------------------------------------------
// The four job verbs
// ---------------------------------------------------------------------------

test("bashBg returns an id and a pid and publishes job.spawned", async () => {
  const r = rig();
  const { id, pid } = JSON.parse(r.registry.bashBg("sleep 60", r.ctx));
  eq(id, "bg_1");
  ok(typeof pid === "number" && pid > 0, "a live pid must come back to the program");
  eq(r.events.length, 1);
  eq(r.events[0].type, "job.spawned");
  eq(r.events[0].sessionId, r.ctx.sessionId);
  const job = r.events[0].data as BackgroundJob;
  eq(job.id, "bg_1");
  eq(job.status, "running");
  eq(job.pid, pid);
  await r.cleanup();
});

test("bashBg refuses past the concurrency cap and names the running ids", async () => {
  const r = rig();
  for (let i = 0; i < 8; i++) r.registry.bashBg("sleep 60", r.ctx);
  const err = throwsWith(() => r.registry.bashBg("sleep 60", r.ctx), ConflictError);
  has(err.message, "bashKill");
  has(err.message, "bg_1");
  await r.cleanup();
});

test("bashOutput returns only what accrued since the last call", async () => {
  const r = rig();
  r.registry.bashBg("printf 'one\\n'; sleep 1.5; printf 'two\\n'; sleep 60", r.ctx);
  const first = await untilAccrued(
    "the first chunk",
    () => r.registry.bashOutput("bg_1", r.ctx.sessionId),
    (s) => s.includes("one"),
  );
  has(first, "[running]");
  const second = await untilAccrued(
    "the second chunk",
    () => r.registry.bashOutput("bg_1", r.ctx.sessionId),
    (s) => s.includes("two"),
  );
  lacks(second, "one", "the cursor must not re-hand output the model already saw");
  await r.cleanup();
});

test("bashWait blocks until exit, returns the exit line, and suppresses the note", async () => {
  const r = rig();
  r.registry.bashBg("printf 'done\\n'; exit 4", r.ctx);
  const out = await r.registry.bashWait("bg_1", r.ctx.sessionId);
  has(out, "done");
  has(out, "[exited with code 4]");
  // Claimed in band — the model already has the result, so nothing wakes it.
  eq(r.notes, []);
  eq(r.events.map((e) => e.type), ["job.spawned", "job.exited"]);
  await r.cleanup();
});

test("an unclaimed noisy exit posts a note; a silent clean one does not", async () => {
  const r = rig();
  r.registry.bashBg("printf 'oops\\n'; exit 2", r.ctx);
  await untilTrue("the completion note", () => r.notes.length === 1);
  eq(r.notes[0].sessionId, r.ctx.sessionId);
  has(r.notes[0].text, "[background] bg_1 finished (exit 2)");
  has(r.notes[0].text, '1 line of output. Read it with bashOutput("bg_1")');

  // A clean, silent, fire-and-forget exit has nothing to report: notifying would
  // wake an idle session into a whole LLM turn just to say "bg_2 finished". The
  // job.exited event still carries the outcome to the jobs panel.
  r.registry.bashBg("exit 0", r.ctx);
  await untilTrue(
    "the second job.exited",
    () => r.events.filter((e) => e.type === "job.exited").length === 2,
  );
  eq(r.notes.length, 1);
  await r.cleanup();
});

test("bashKill reports the real outcome and a second kill reports the prior exit", async () => {
  const r = rig();
  r.registry.bashBg("sleep 60", r.ctx);
  matches(await r.registry.bashKill("bg_1", r.ctx.sessionId), /^killed bg_1 \(/);
  matches(await r.registry.bashKill("bg_1", r.ctx.sessionId), /^bg_1 already exited/);
  // A deliberate kill is claimed: it must not also wake the model with a note.
  eq(r.notes, []);
  await r.cleanup();
});

test("an unknown job id says what this session actually has", async () => {
  const r = rig();
  const empty = throwsWith(
    () => r.registry.bashOutput("bg_9", r.ctx.sessionId),
    NotFoundError,
  );
  has(empty.message, "has started none");
  has(empty.message, "bashBg");

  r.registry.bashBg("sleep 60", r.ctx);
  const known = throwsWith(
    () => r.registry.bashOutput("bg_9", r.ctx.sessionId),
    NotFoundError,
  );
  has(known.message, "this session has bg_1");
  await r.cleanup();
});

test("a session cannot see or read another session's shells", async () => {
  const r = rig({ sessionId: "sess-a" });
  r.registry.bashBg("sleep 60", r.ctx);
  eq(r.registry.listJobs("sess-b"), []);
  throwsWith(() => r.registry.bashOutput("bg_1", "sess-b"), NotFoundError);
  // The jobs API, by contrast, reaches across sessions on purpose: anything the UI
  // can list it must also be able to read and kill.
  eq(r.registry.jobOutput("bg_1")!.job.sessionId, "sess-a");
  await r.cleanup();
});

test("jobOutput does not steal the model's bashOutput cursor", async () => {
  const r = rig();
  r.registry.bashBg("printf 'shared\\n'; sleep 60", r.ctx);
  await untilTrue(
    "the UI read",
    () => (r.registry.jobOutput("bg_1")?.output ?? "").includes("shared"),
  );
  // A human looked; the model has still never read this shell.
  has(r.registry.bashOutput("bg_1", r.ctx.sessionId), "shared");
  await r.cleanup();
});

test("killJobsOf stops one session's shells and leaves another's alone", async () => {
  const r = rig({ sessionId: "sess-a" });
  const other: ShellCtx = { sessionId: "sess-b", workspace: process.cwd() };
  r.registry.bashBg("sleep 60", r.ctx);
  r.registry.bashBg("sleep 60", r.ctx);
  r.registry.bashBg("sleep 60", other);
  eq(r.registry.killJobsOf("sess-a"), 2);
  await untilTrue("both exits", () => r.registry.runningIds("sess-a").length === 0);
  eq(r.registry.runningIds("sess-b").length, 1);
  eq(r.registry.killAll(), 1);
  await untilTrue("the last exit", () => r.registry.runningIds("sess-b").length === 0);
  await r.cleanup();
});

// ---------------------------------------------------------------------------
// Deterministic truncation
// ---------------------------------------------------------------------------

test("truncateMiddle keeps head and tail verbatim with an explicit marker", () => {
  const text = "H".repeat(50) + "M".repeat(500) + "T".repeat(50);
  const out = truncateMiddle(text, { head: 50, tail: 50 });
  ok(out.startsWith("H".repeat(50)), "the head must survive verbatim");
  ok(out.endsWith("T".repeat(50)), "the tail must survive verbatim");
  lacks(out, "M", "the middle is what gets omitted");
  has(out, "500 chars omitted from the middle of 600");
  has(out, "head and tail are verbatim");
  // No LLM, no sampling: the same input always produces the same output.
  eq(truncateMiddle(text, { head: 50, tail: 50 }), out);
});

test("truncateMiddle leaves anything within budget completely untouched", () => {
  const text = "x".repeat(100);
  eq(truncateMiddle(text, { head: 50, tail: 50 }), text);
  eq(truncateMiddle("", { head: 1, tail: 1 }), "");
  eq(MAX_HEAD_CHARS + MAX_TAIL_CHARS, 400_000);
});

test("a shell's retained buffer keeps the HEAD when output overruns the budget", async () => {
  const registry = new JobRegistry({ limits: { head: 40, tail: 40 } });
  const shell = registry.spawn(MANY_LINES, { cwd: process.cwd() });
  await shell.exit;
  await shell.pumps;
  const text = shellText(shell);
  // The head is the FIRST bytes the command printed, not the last — a rolling
  // buffer that dropped the oldest would silently rewrite what was already seen.
  ok(text.startsWith("line1\n"), `expected the verbatim head, got:\n${text}`);
  ok(text.trimEnd().endsWith("line200"), `expected the verbatim tail, got:\n${text}`);
  has(text, "chars omitted from the middle");
  lacks(text, "line100", "the middle is what gets omitted");
});

test("bashOutput reports the hole when unread output falls out of retention", async () => {
  const registry = new JobRegistry({ limits: { head: 20, tail: 20 } });
  const ctx: ShellCtx = { sessionId: "sess-hole", workspace: process.cwd() };
  registry.bashBg(MANY_LINES, ctx);
  const seen = await registry.bashWait("bg_1", ctx.sessionId);
  has(seen, "chars omitted from the middle");
  has(seen, "[exited with code 0]");
  await registry.drain();
});

// ---------------------------------------------------------------------------
// The bridged surface
// ---------------------------------------------------------------------------

test("the bridged sh takes a JSON array and answers with JSON, in order", async () => {
  const r = rig();
  const host = createShellHostFns(r.ctx, { registry: r.registry });
  const out = await host.sh(JSON.stringify(["printf 'a\\n'", "exit 5", "printf 'c\\n'"]));
  eq(JSON.parse(out), [
    { code: 0, out: "a" },
    { code: 5, out: "" },
    { code: 0, out: "c" },
  ]);
  await r.cleanup();
});

test("the bridged sh rejects a non-array payload with the call it wanted", async () => {
  const r = rig();
  const host = createShellHostFns(r.ctx, { registry: r.registry });
  for (const bad of ['"ls"', "not json at all", "[1,2]"]) {
    const err = await rejectsWith(() => host.sh(bad), ProgramError);
    has(err.message, 'sh("cmd one", "cmd two")');
  }
  await r.cleanup();
});

test("the bridged job verbs round-trip through the registry", async () => {
  const r = rig();
  const host = createShellHostFns(r.ctx, { registry: r.registry });
  const { id } = JSON.parse(await host.bashBg("printf 'bridged\\n'; exit 0"));
  eq(id, "bg_1");
  const waited = await host.bashWait(id);
  has(waited, "bridged");
  has(waited, "[exited with code 0]");
  has(await host.bashOutput(id), "(no new output)");
  matches(await host.bashKill(id), /already exited/);
  await r.cleanup();
});
