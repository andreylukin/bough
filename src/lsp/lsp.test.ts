/**
 * These tests are almost entirely about ONE distinction: the difference between "the
 * backend looked and found nothing" and "the backend is broken" (spec §10, plan
 * §6.14, T7.3's acceptance criterion). Everything else here — argv shapes, argument
 * validation, the JSON envelope — is ordinary plumbing that would be caught the first
 * time anyone used it. The distinction would not: both look like a verb that did not
 * produce an answer, and getting it wrong is silent in both directions.
 *
 *   - Empty read as broken → the agent retires symbol navigation for the whole task
 *     because someone misspelled a symbol.
 *   - Broken read as empty → the agent concludes the symbol has no callers and edits
 *     a signature whose call sites it never saw.
 *
 * So: `classify` is tested case by case as a pure function; the bridge is tested for
 * the two paths end to end; and the backend-error path is tested for the three things
 * the prompt depends on — it is CATCHABLE (an ordinary exception inside the program,
 * not a killed worker), it is REPORTED ONCE, and it does not run the backend again
 * for the rest of the turn.
 *
 * Hermetic and offline. The backend binary does not exist in this container and is
 * never looked for: every test injects a fake `run`, and the two that exercise binary
 * discovery inject the environment and the stat. Nothing spawns a process, binds a
 * socket, or reads `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * denied by this environment's egress policy, so the jsr import declared in
 * `deno.json` cannot resolve. `node:assert` is built into the runtime and needs no
 * fetch. (Same constraint `db.test.ts`, `patch.test.ts` and `state.test.ts` document.)
 *
 * `hostfn/lsp.ts` is tested here rather than in a sibling file because it is a
 * transport over this module — an envelope parse and a `JSON.stringify` — and the
 * behaviour it has to preserve is the behaviour under test above it.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { HOST_FN_VERBS } from "../harness/protocol.ts";
import { LspError } from "../errors.ts";
import { createLspHostFn } from "../hostfn/lsp.ts";
import type { TurnCtx } from "../types.ts";
import {
  buildArgv,
  classify,
  createLspBridge,
  findBackend,
  LSP_VERBS,
  lspAvailable,
  type LspExec,
  type LspRun,
} from "./lsp.ts";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

const ok = (stdout: string): LspExec => ({ code: 0, stdout, stderr: "" });
const fails = (code: number, stderr: string, stdout = ""): LspExec => ({ code, stdout, stderr });

/** A scripted backend: one response per invocation, plus the argv it was given. */
function fakeBackend(script: (LspExec | Error)[]): { run: LspRun; calls: string[][] } {
  const calls: string[][] = [];
  let i = 0;
  const run: LspRun = (args) => {
    calls.push(args);
    const next = script[Math.min(i, script.length - 1)];
    i++;
    return next instanceof Error ? Promise.reject(next) : Promise.resolve(next);
  };
  return { run, calls };
}

/** `workspace add` succeeds, then the script answers the verbs. */
function backend(script: (LspExec | Error)[]) {
  return fakeBackend([ok(""), ...script]);
}

async function caught(fn: () => Promise<unknown>): Promise<Error> {
  try {
    await fn();
  } catch (err) {
    return err as Error;
  }
  throw new Error("expected a rejection, got a resolved value");
}

// ---------------------------------------------------------------------------
// classify — the pure decision
// ---------------------------------------------------------------------------

test("classify: exit 0 with output is an answer", () => {
  assert.deepEqual(classify(ok("src/gate.ts:12  decide()")), {
    kind: "text",
    text: "src/gate.ts:12  decide()",
  });
});

test("classify: exit 0 with no output is EMPTY, not an error", () => {
  assert.deepEqual(classify(ok("")), { kind: "empty" });
  assert.deepEqual(classify(ok("   \n ")), { kind: "empty" });
});

test("classify: a grep-shaped non-zero exit for no matches is EMPTY", () => {
  // The single most common non-zero exit a navigation CLI produces, and the case
  // the ported implementation reported as a broken backend.
  assert.deepEqual(classify(fails(1, "no matches")), { kind: "empty" });
  assert.deepEqual(classify(fails(1, "symbol not found: Gate.decide")), { kind: "empty" });
  assert.deepEqual(classify(fails(1, "")), { kind: "empty" });
  assert.deepEqual(classify(fails(1, "", "No references found")), { kind: "empty" });
});

test("classify: backend phrases are a BACKEND failure", () => {
  for (
    const said of [
      "leta: command not found",
      "language server for typescript failed to start",
      "could not start the daemon: connection refused",
      "panic: runtime error",
      "timed out waiting for the index",
    ]
  ) {
    const outcome = classify(fails(1, said));
    assert.equal(outcome.kind, "backend", `expected backend for ${said}`);
  }
});

test("classify: exit codes that mean the binary did not run are BACKEND", () => {
  assert.equal(classify(fails(127, "")).kind, "backend");
  assert.equal(classify(fails(126, "")).kind, "backend");
  assert.equal(classify(fails(139, "")).kind, "backend"); // SIGSEGV
});

test("classify: a bad query is neither empty nor a backend failure", () => {
  // The backend answered. Retiring the verbs over this would be the worst outcome
  // of the three, so ambiguity and bad paths get their own class.
  assert.equal(
    classify(fails(2, "ambiguous symbol decide; candidates: Gate.decide")).kind,
    "query",
  );
  assert.equal(classify(fails(2, "open src/typo.ts: no such file or directory")).kind, "query");
  assert.equal(classify(fails(2, "usage: leta show <symbol>")).kind, "query");
});

test("classify: an unexplained non-zero exit is treated as a backend failure", () => {
  // Documented default: a slower correct answer (drop to rg) beats a confident
  // wrong one ("this symbol has no callers").
  const outcome = classify(fails(3, "wat"));
  assert.equal(outcome.kind, "backend");
  assert.match((outcome as { detail: string }).detail, /wat/);
});

// ---------------------------------------------------------------------------
// the verb surface
// ---------------------------------------------------------------------------

test("the curated verbs are exactly the protocol's list", () => {
  // The worker rebuilds `lsp.*` from `HOST_FN_VERBS.lsp`; a verb here that is not
  // there is unreachable, and one there that is not here rejects at runtime.
  assert.deepEqual([...LSP_VERBS], [...HOST_FN_VERBS.lsp]);
  for (const verb of LSP_VERBS) {
    assert.ok(Array.isArray(buildArgv(verb, argsFor(verb))), `${verb} builds no argv`);
  }
});

function argsFor(verb: string): unknown {
  switch (verb) {
    case "find":
      return { pattern: "Gate" };
    case "overview":
      return { path: "src/gate.ts" };
    case "calls":
      return { to: "Gate.decide" };
    case "rename":
      return { symbol: "Gate.decide", new_name: "Gate.choose" };
    default:
      return { symbol: "Gate.decide" };
  }
}

test("buildArgv maps bough verbs onto the backend's own subcommands", () => {
  assert.deepEqual(buildArgv("find", { pattern: "Gate", path: "src/" }), [
    "grep",
    "Gate",
    "src/",
  ]);
  assert.deepEqual(buildArgv("overview", { path: "src/gate.ts" }), ["grep", ".", "src/gate.ts"]);
  assert.deepEqual(buildArgv("def", { symbol: "Gate.decide" }), ["declaration", "Gate.decide"]);
  assert.deepEqual(buildArgv("refs", { symbol: "Gate.decide", context: 2 }), [
    "refs",
    "Gate.decide",
    "--context",
    "2",
  ]);
  assert.deepEqual(buildArgv("calls", { from: "Gate.decide" }), ["calls", "--from", "Gate.decide"]);
});

test("buildArgv accepts a bare string for the single-argument verbs", () => {
  assert.deepEqual(buildArgv("def", "Gate.decide"), ["declaration", "Gate.decide"]);
  assert.deepEqual(buildArgv("find", "Gate"), ["grep", "Gate"]);
  // `calls` is excluded on purpose: a bare string cannot say to-vs-from.
  const err = await0(() => buildArgv("calls", "Gate.decide"));
  assert.equal((err as LspError).status, 400);
});

function await0(fn: () => unknown): unknown {
  try {
    fn();
  } catch (err) {
    return err;
  }
  throw new Error("expected a throw");
}

test("buildArgv rejects a bad call with the shape that would have worked", () => {
  const missing = await0(() => buildArgv("show", {})) as LspError;
  assert.equal(missing.status, 400);
  assert.match(missing.message, /lsp\.show\(\{symbol/);

  const both = await0(() => buildArgv("calls", { to: "a", from: "b" })) as LspError;
  assert.match(both.message, /exactly one of/);
  assert.match(both.message, /both were given/);

  const unknown = await0(() => buildArgv("outline", {})) as LspError;
  assert.match(unknown.message, /unknown lsp verb "outline"/);
  assert.match(unknown.message, /find, show, def/);
});

// ---------------------------------------------------------------------------
// the bridge: empty vs backend error
// ---------------------------------------------------------------------------

test("bridge: nothing runs until the first call (lazy)", async () => {
  const { run, calls } = backend([ok("hit")]);
  const bridge = createLspBridge({ workspace: "/w", run });
  assert.equal(calls.length, 0, "constructing the bridge invoked the backend");
  await bridge.call("def", { symbol: "Gate.decide" });
  assert.ok(calls.length > 0);
});

test("bridge: the workspace is registered once, before the first verb", async () => {
  const { run, calls } = backend([ok("a"), ok("b")]);
  const bridge = createLspBridge({ workspace: "/w", run });
  await bridge.call("def", { symbol: "A" });
  await bridge.call("def", { symbol: "B" });
  assert.deepEqual(calls[0], ["workspace", "add"]);
  assert.equal(calls.filter((c) => c[0] === "workspace").length, 1);
});

test("bridge: an EMPTY result resolves as an ordinary answer", async () => {
  const { run } = backend([ok("")]);
  const bridge = createLspBridge({ workspace: "/w", run });

  // Resolves. It does not reject, and that is the whole assertion.
  const answer = await bridge.call("refs", { symbol: "Gate.decide" });
  assert.match(answer, /no results/);
  assert.match(answer, /ordinary answer, not a failure/);
  assert.match(answer, /Gate\.decide/);
  assert.equal(bridge.down, undefined, "an empty result must not mark the backend down");
});

test("bridge: an empty result leaves every later verb working", async () => {
  // The behaviour the prompt promises: "keep using the verbs for the next lookup".
  const { run, calls } = backend([ok(""), ok("src/gate.ts:12")]);
  const bridge = createLspBridge({ workspace: "/w", run });
  await bridge.call("refs", { symbol: "Nope" });
  const second = await bridge.call("def", { symbol: "Gate.decide" });
  assert.equal(second, "src/gate.ts:12");
  assert.equal(calls.length, 3); // register + two verbs
});

test("bridge: a BACKEND error rejects, catchably, saying to drop to rg", async () => {
  const { run } = backend([fails(1, "language server failed to start")]);
  const bridge = createLspBridge({ workspace: "/w", run });

  const err = await caught(() => bridge.call("refs", { symbol: "Gate.decide" }));
  // Catchable: an ordinary exception the program's own try/catch sees.
  assert.ok(err instanceof Error);
  assert.ok(err instanceof LspError);
  assert.equal((err as LspError).status, 502);
  // It says BACKEND, not "no such symbol" — spec §6's error table.
  assert.match(err.message, /BACKEND failed/);
  assert.match(err.message, /language server failed to start/);
  assert.match(err.message, /rg \+ view \+ patch/);
  assert.match(err.message, /not treat it as blocking/i);
  assert.equal(bridge.down, "language server failed to start");
});

test("bridge: the backend failure is reported ONCE and never re-run", async () => {
  const reports: string[] = [];
  const { run, calls } = backend([fails(127, "leta: command not found")]);
  const bridge = createLspBridge({
    workspace: "/w",
    run,
    onBackendDown: (detail) => reports.push(detail),
  });

  const first = await caught(() => bridge.call("refs", { symbol: "A" }));
  const afterFirst = calls.length;

  // Three more verbs, exactly as a model that did not read the message would issue.
  const later = [
    await caught(() => bridge.call("def", { symbol: "A" })),
    await caught(() => bridge.call("find", { pattern: "A" })),
    await caught(() => bridge.call("overview", { path: "src/" })),
  ];

  assert.deepEqual(reports, ["leta: command not found"], "reported more than once");
  assert.equal(calls.length, afterFirst, "the backend was invoked again after it failed");
  assert.match(first.message, /BACKEND failed/);
  for (const err of later) {
    assert.equal((err as LspError).status, 502);
    assert.match(err.message, /already failed this turn/);
    assert.match(err.message, /rg \+ view \+ patch/);
  }
});

test("bridge: a thrown invocation (no binary, spawn refused) is a backend failure", async () => {
  const { run } = backend([new Error("No such file or directory (os error 2)")]);
  const bridge = createLspBridge({ workspace: "/w", run });
  const err = await caught(() => bridge.call("def", { symbol: "A" }));
  assert.equal((err as LspError).status, 502);
  assert.match(err.message, /BACKEND failed/);
});

test("bridge: a failed workspace registration is a backend failure, reported once", async () => {
  const reports: string[] = [];
  const { run, calls } = fakeBackend([fails(1, "could not start the daemon")]);
  const bridge = createLspBridge({
    workspace: "/w",
    run,
    onBackendDown: (d) => reports.push(d),
  });
  const first = await caught(() => bridge.call("def", { symbol: "A" }));
  assert.match(first.message, /BACKEND failed/);
  const second = await caught(() => bridge.call("refs", { symbol: "A" }));
  assert.match(second.message, /already failed this turn/);
  assert.equal(reports.length, 1);
  assert.equal(calls.length, 1, "registration was retried against a dead backend");
});

test("bridge: a QUERY error does not retire the backend", async () => {
  const { run } = backend([
    fails(2, "ambiguous symbol decide; candidates: Gate.decide, Lock.decide"),
    ok("src/gate.ts:12"),
  ]);
  const bridge = createLspBridge({ workspace: "/w", run });

  const err = await caught(() => bridge.call("def", { symbol: "decide" }));
  assert.equal((err as LspError).status, 400);
  assert.match(err.message, /ambiguous/);
  assert.match(err.message, /backend is working/);
  assert.equal(bridge.down, undefined);

  // And the next lookup goes through.
  assert.equal(await bridge.call("def", { symbol: "Gate.decide" }), "src/gate.ts:12");
});

test("bridge: a bad argument never reaches the backend and never latches", async () => {
  const { run, calls } = backend([ok("src/gate.ts:12")]);
  const bridge = createLspBridge({ workspace: "/w", run });

  const err = await caught(() => bridge.call("show", { symbl: "typo" }));
  assert.equal((err as LspError).status, 400);
  assert.equal(calls.length, 0, "a malformed call spawned the backend");
  assert.equal(bridge.down, undefined);

  assert.equal(await bridge.call("def", { symbol: "Gate.decide" }), "src/gate.ts:12");
});

test("bridge: an interrupted turn is not a backend failure", async () => {
  const controller = new AbortController();
  controller.abort();
  const { run, calls } = backend([ok("never")]);
  const bridge = createLspBridge({ workspace: "/w", run, signal: controller.signal });

  const err = await caught(() => bridge.call("def", { symbol: "A" }));
  assert.equal((err as LspError).status, 400);
  assert.match(err.message, /interrupted/);
  assert.match(err.message, /backend itself is fine/);
  assert.equal(calls.length, 0, "an interrupted turn spawned the backend anyway");
  assert.equal(bridge.down, undefined, "an interrupt must not retire the backend");
});

test("bridge: the call runs in the session's workspace", async () => {
  const seen: string[] = [];
  const run: LspRun = (_args, opts) => {
    seen.push(opts.cwd);
    return Promise.resolve(ok(""));
  };
  await createLspBridge({ workspace: "/checkout", run }).call("find", { pattern: "x" });
  assert.deepEqual(seen, ["/checkout", "/checkout"]);
});

// ---------------------------------------------------------------------------
// binary discovery — stats only, never a spawn
// ---------------------------------------------------------------------------

test("findBackend: an explicit BOUGH_LSP_BIN wins over the PATH scan", () => {
  const env = (n: string) =>
    ({ BOUGH_LSP_BIN: "/opt/custom/leta", PATH: "/usr/bin" } as Record<string, string>)[n];
  assert.equal(findBackend({ env, isFile: (p) => p === "/opt/custom/leta" }), "/opt/custom/leta");
  // An override pointing at nothing resolves to nothing rather than silently
  // falling back — the user said where it is.
  assert.equal(findBackend({ env, isFile: (p) => p === "/usr/bin/leta" }), undefined);
});

test("findBackend: PATH first, then the dirs a launchd PATH omits", () => {
  const env = (n: string) => (n === "PATH" ? "/nope:/usr/local/bin" : undefined);
  assert.equal(
    findBackend({ env, isFile: (p) => p === "/usr/local/bin/leta" }),
    "/usr/local/bin/leta",
  );
  assert.equal(
    findBackend({ env: () => undefined, isFile: (p) => p === "/opt/homebrew/bin/leta" }),
    "/opt/homebrew/bin/leta",
  );
  assert.equal(findBackend({ env, isFile: () => false }), undefined);
  assert.equal(lspAvailable({ env, isFile: () => false }), false);
});

// ---------------------------------------------------------------------------
// the host function: the string-only wire
// ---------------------------------------------------------------------------

function turnCtx(over: Partial<TurnCtx> = {}): TurnCtx {
  return {
    db: null as unknown as TurnCtx["db"],
    bus: null as unknown as TurnCtx["bus"],
    sessionId: "s1",
    turnId: "t1",
    messageId: "m1",
    workspace: "/w",
    model: "test",
    signal: new AbortController().signal,
    depth: 0,
    ...over,
  };
}

test("hostfn: a result crosses the wire JSON-encoded, as the worker expects", async () => {
  const { run } = backend([ok("src/gate.ts:12  decide()")]);
  const { lsp } = createLspHostFn(turnCtx(), { run });
  const wire = await lsp!("def", JSON.stringify({ symbol: "Gate.decide" }));
  // `harness/vm_worker.ts` does `JSON.parse` on every verb-dispatched result.
  assert.equal(JSON.parse(wire), "src/gate.ts:12  decide()");
});

test("hostfn: an empty result resolves; a dead backend rejects", async () => {
  const empty = createLspHostFn(turnCtx(), { run: backend([ok("")]).run });
  const answer = JSON.parse(await empty.lsp!("refs", JSON.stringify({ symbol: "A" })));
  assert.match(answer, /ordinary answer, not a failure/);

  const reports: string[] = [];
  const dead = createLspHostFn(turnCtx(), {
    run: backend([fails(1, "language server failed to start")]).run,
    onBackendDown: (d) => reports.push(d),
  });
  const err = await caught(() => dead.lsp!("refs", JSON.stringify({ symbol: "A" })));
  assert.match(err.message, /BACKEND failed/);
  assert.equal(reports.length, 1);
});

test("hostfn: one bridge per turn — the latch spans calls", async () => {
  const reports: string[] = [];
  const { run, calls } = backend([fails(127, "leta: command not found")]);
  const { lsp } = createLspHostFn(turnCtx(), { run, onBackendDown: (d) => reports.push(d) });
  await caught(() => lsp!("def", JSON.stringify({ symbol: "A" })));
  const after = calls.length;
  await caught(() => lsp!("refs", JSON.stringify({ symbol: "A" })));
  assert.equal(calls.length, after);
  assert.deepEqual(reports, ["leta: command not found"]);
});

test("hostfn: no-argument and malformed envelopes are verb errors", async () => {
  const { run, calls } = backend([ok("x")]);
  const { lsp } = createLspHostFn(turnCtx(), { run });

  // The worker sends `JSON.stringify(args ?? null)` — "null" for `lsp.def()`.
  const bare = await caught(() => lsp!("def", "null"));
  assert.equal((bare as LspError).status, 400);
  assert.match(bare.message, /symbol/);

  const broken = await caught(() => lsp!("def", "{not json"));
  assert.equal((broken as LspError).status, 400);
  assert.match(broken.message, /could not be read as JSON/);

  assert.equal(calls.length, 0, "a malformed envelope reached the backend");
});

test("hostfn: constructing it touches nothing", () => {
  // Every turn builds one, including the ones that never ask about a symbol.
  const { run, calls } = backend([ok("x")]);
  createLspHostFn(turnCtx(), { run });
  assert.equal(calls.length, 0);
});
