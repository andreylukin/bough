/**
 * Tests for `bough exec`.
 *
 * The load-bearing one is "a turn that finishes inside the post is still seen",
 * and its whole design is about being able to FAIL. It drives the real route
 * table (`server/app.ts`) over an in-memory database, with a turn starter that
 * publishes `message.delta` and `turn.finished` SYNCHRONOUSLY inside the post
 * handler — so by the time `POST /sessions/:id/messages` returns, the turn is
 * over. `/events` has no replay, so a client that subscribes afterwards observes
 * nothing at all. `postFirst` below is the inverted implementation, run against
 * the same fixture, and it is asserted to see nothing: that is the proof that the
 * ordering test discriminates rather than passing for free.
 *
 * Everything here is offline and hermetic — no socket is bound, no port claimed,
 * nothing reads `~/.bough`. `node:assert/strict` because jsr.io is unreachable.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { createHandler } from "../server/app.ts";
import type { WithTurnStarter } from "../server/sessions.ts";
import type { AppCtx } from "../types.ts";
import { TurnRegistry } from "../turn/queue.ts";
import type { Message, Session, TurnStatus } from "../schema/parts.ts";
import {
  createSseReader,
  type ExecDeps,
  type ExecEnvelope,
  isHelpRequest,
  isUsageError,
  parseExecArgs,
  runExec,
  USAGE,
} from "./exec.ts";

// ---- fixture -------------------------------------------------------------------

/** What the fabricated turn does when a prompt lands. */
type FakeTurn = (ctx: AppCtx, session: Session, message: Message) => void;

/** Publishes some assistant text, then finishes the turn — all synchronously. */
function instantTurn(text: string, status: TurnStatus = "done", error?: string): FakeTurn {
  return (ctx, session) => {
    const messageId = crypto.randomUUID();
    if (text) {
      ctx.bus.publish({
        type: "message.delta",
        sessionId: session.id,
        data: { messageId, delta: text },
      });
    }
    ctx.bus.publish({
      type: "turn.finished",
      sessionId: session.id,
      data: {
        turnId: crypto.randomUUID(),
        sessionId: session.id,
        status,
        ...(error ? { error } : {}),
      },
    });
  };
}

interface Fixture {
  deps: ExecDeps;
  /** Method + path of every request, in the order the client made them. */
  calls: string[];
  out: () => string;
  err: () => string;
  ctx: AppCtx & WithTurnStarter;
  close: () => void;
}

function fixture(options: {
  turn?: FakeTurn;
  stdin?: string;
  isTerminal?: boolean;
  cwd?: string;
  env?: Record<string, string>;
  /** Extra ctx seams — the interrupt route reads `turnRegistry` off it. */
  ctxExtra?: Record<string, unknown>;
} = {}): Fixture {
  const db = openDb(":memory:");
  const ctx: AppCtx & WithTurnStarter = { db, bus: new Bus(), ...options.ctxExtra };
  if (options.turn) {
    ctx.startTurn = (appCtx, session, message) => options.turn!(appCtx, session, message);
  }
  const handler = createHandler(ctx, { onUnexpectedError: () => {} });

  const calls: string[] = [];
  let out = "";
  let err = "";

  const deps: ExecDeps = {
    fetchFn: (input, init) => {
      const req = new Request(input as string | URL, init);
      calls.push(`${req.method} ${new URL(req.url).pathname}`);
      return handler(req);
    },
    write: (text) => {
      out += text;
    },
    warn: (text) => {
      err += text + "\n";
    },
    readStdin: () => Promise.resolve(options.stdin ?? ""),
    stdinIsTerminal: () => options.isTerminal ?? true,
    env: (name) => options.env?.[name],
    cwd: () => options.cwd ?? "/tmp",
    realPath: (path) => realpath(path),
  };

  return { deps, calls, out: () => out, err: () => err, ctx, close: () => db.close() };
}

// ---- THE ordering test ----------------------------------------------------------

test("a turn that finishes inside the post is still seen — stream before post", async () => {
  const f = fixture({ turn: instantTurn("the answer") });
  try {
    const code = await runExec(["--timeout", "1", "do the thing"], f.deps);

    assert.equal(code, 0, `expected a completed turn; stderr was: ${f.err()}`);
    assert.equal(f.out(), "the answer\n");

    // The ordering itself, stated as a fact about the call sequence. `/events`
    // must come between creating the session and posting the prompt.
    const events = f.calls.indexOf("GET /events");
    const post = f.calls.findIndex((c) =>
      c.startsWith("POST /sessions/") && c.endsWith("/messages")
    );
    assert.ok(events !== -1, `no /events call at all: ${f.calls.join(", ")}`);
    assert.ok(post !== -1, `no message post at all: ${f.calls.join(", ")}`);
    assert.ok(
      events < post,
      `the event stream must be open BEFORE the prompt is posted, got: ${f.calls.join(", ")}`,
    );
  } finally {
    f.close();
  }
});

/**
 * The inverted client: post, THEN subscribe. Not production code — it exists to
 * demonstrate that the test above fails when the ordering is reversed, which is
 * the only thing that makes that test worth having.
 */
async function postFirst(deps: ExecDeps): Promise<{ sawFinish: boolean; text: string }> {
  const base = "http://127.0.0.1:4321";
  const created = await deps.fetchFn(`${base}/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ title: "inverted" }),
  });
  const session = await created.json() as Session;

  await deps.fetchFn(`${base}/sessions/${session.id}/messages`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ text: "do the thing" }),
  });

  const events = await deps.fetchFn(`${base}/events?sessionId=${session.id}`);
  const reader = events.body!.pipeThrough(new TextDecoderStream()).getReader();
  const feed = createSseReader();

  // Bounded, because the whole point is that nothing is coming.
  const stop = new Promise<null>((r) => setTimeout(() => r(null), 150));
  let sawFinish = false;
  let text = "";
  for (;;) {
    const chunk = await Promise.race([reader.read(), stop]);
    if (chunk === null || chunk.done || chunk.value === undefined) break;
    for (const frame of feed(chunk.value)) {
      const data = (frame.data as { data?: Record<string, string> }).data ?? {};
      if (frame.name === "message.delta") text += data.delta ?? "";
      if (frame.name === "turn.finished") sawFinish = true;
    }
    if (sawFinish) break;
  }
  await reader.cancel().catch(() => {});
  return { sawFinish, text };
}

test("proof the ordering test discriminates: post-then-subscribe sees nothing", async () => {
  const f = fixture({ turn: instantTurn("the answer") });
  try {
    const observed = await postFirst(f.deps);
    assert.equal(
      observed.sawFinish,
      false,
      "the inverted ordering observed turn.finished — this fixture no longer proves anything",
    );
    assert.equal(observed.text, "", "the inverted ordering observed the assistant text");
  } finally {
    f.close();
  }
});

// ---- exit codes ------------------------------------------------------------------

test("exit 0: a completed turn", async () => {
  const f = fixture({ turn: instantTurn("done here") });
  try {
    assert.equal(await runExec(["--timeout", "1", "go"], f.deps), 0);
    assert.equal(f.out(), "done here\n");
  } finally {
    f.close();
  }
});

test("exit 1: an errored turn, with the server's reason on stderr", async () => {
  const f = fixture({ turn: instantTurn("partial", "error", "context window exceeded: 200000") });
  try {
    assert.equal(await runExec(["--timeout", "1", "go"], f.deps), 1);
    assert.match(f.err(), /context window exceeded: 200000/);
    // The partial answer still reaches stdout — it is what the model actually said.
    assert.equal(f.out(), "partial\n");
  } finally {
    f.close();
  }
});

test("exit 1: an interrupted or orphaned turn is not a completed turn", async () => {
  for (const status of ["interrupted", "orphaned"] as const) {
    const f = fixture({ turn: instantTurn("", status) });
    try {
      assert.equal(await runExec(["--timeout", "1", "go"], f.deps), 1, status);
    } finally {
      f.close();
    }
  }
});

test("exit 1: the timeout elapses, and the abandoned turn is INTERRUPTED", async () => {
  // A turn that starts and never reports. `--timeout` is fractional so the test
  // costs milliseconds rather than the 900s default.
  //
  // The registry is registered against, so the interrupt the client raises has a real
  // turn to signal — which is the whole point: a `--timeout` that walks away without
  // stopping the turn leaves it running and spending, and the next command against
  // that session queues behind it (spec §5).
  const registry = new TurnRegistry();
  let controller: AbortController | undefined;
  const f = fixture({
    turn: (_ctx, session) => {
      controller = registry.begin(session.id);
    },
    ctxExtra: { turnRegistry: registry },
  });
  try {
    assert.equal(await runExec(["--timeout", "0.15", "go"], f.deps), 1);
    assert.match(f.err(), /timed out after 0\.15s/);
    assert.match(f.err(), /interrupted the turn/);
    assert.ok(
      f.calls.some((c) => c.startsWith("POST /sessions/") && c.endsWith("/interrupt")),
      `the client must raise the interrupt it promises: ${f.calls.join(", ")}`,
    );
    assert.equal(controller?.signal.aborted, true, "the turn's controller must be aborted");
  } finally {
    f.close();
  }
});

test("exit 1: a stop that finds nothing running says so, and still exits 1", async () => {
  // The race: the turn ended between the stream closing and the stop being raised.
  // The client must not claim it interrupted anything, and must not change its mind
  // about the exit code — a turn it gave up on did not complete either way.
  const f = fixture({ turn: () => {}, ctxExtra: { turnRegistry: new TurnRegistry() } });
  try {
    assert.equal(await runExec(["--timeout", "0.15", "go"], f.deps), 1);
    assert.match(f.err(), /could NOT interrupt/);
  } finally {
    f.close();
  }
});

test("exit 2: no server on the port", async () => {
  const f = fixture();
  try {
    const deps: ExecDeps = {
      ...f.deps,
      fetchFn: () => Promise.reject(new TypeError("connection refused")),
    };
    assert.equal(await runExec(["--port", "4399", "go"], deps), 2);
    assert.match(f.err(), /cannot reach bough on :4399/);
    assert.match(f.err(), /connection refused/);
  } finally {
    f.close();
  }
});

test("exit 2: the server refuses the session", async () => {
  const f = fixture();
  try {
    // A workspace that is not a directory is a 400 from `POST /sessions` — a
    // usage problem, reported as one rather than as a turn failure.
    const code = await runExec(["-w", "/tmp", "go"], {
      ...f.deps,
      realPath: () => Promise.resolve("/definitely/not/a/directory"),
    });
    assert.equal(code, 2);
    assert.match(f.err(), /bough refused the session: 400/);
  } finally {
    f.close();
  }
});

test("exit 2: no prompt, and no piped stdin to take one from", async () => {
  const f = fixture({ isTerminal: true });
  try {
    assert.equal(await runExec([], f.deps), 2);
    assert.equal(f.err().trim(), USAGE);
    assert.deepEqual(f.calls, [], "nothing is created before the prompt is known");
  } finally {
    f.close();
  }
});

test("exit 2: an unknown flag stops rather than streaming", async () => {
  const f = fixture();
  try {
    assert.equal(await runExec(["--jsno", "go"], f.deps), 2);
    assert.match(f.err(), /unknown flag --jsno/);
  } finally {
    f.close();
  }
});

// ---- --json ------------------------------------------------------------------------

test("--json suppresses streaming and prints one envelope carrying the text", async () => {
  const f = fixture({ turn: instantTurn("hello there") });
  try {
    assert.equal(await runExec(["--json", "--timeout", "1", "go"], f.deps), 0);

    const lines = f.out().trimEnd().split("\n");
    assert.equal(lines.length, 1, `expected exactly one line, got: ${JSON.stringify(f.out())}`);
    const envelope = JSON.parse(lines[0]) as ExecEnvelope;
    assert.equal(envelope.status, "done");
    assert.equal(envelope.ok, true);
    // Suppressed from stdout, not discarded: `--json` still answers the question.
    assert.equal(envelope.text, "hello there");
    assert.ok(envelope.session.length > 0);
    // Usage rides along from `GET /sessions/:id`, the authoritative post-turn record.
    assert.equal(typeof envelope.usage?.inputTokens, "number");
    assert.equal(typeof envelope.treeUsage?.costUsd, "number");
    assert.equal(f.calls.filter((c) => c === "GET /sessions/" + envelope.session).length, 1);
  } finally {
    f.close();
  }
});

test("--json on a failed turn is still one envelope, with ok false", async () => {
  const f = fixture({ turn: instantTurn("", "error", "provider 500") });
  try {
    assert.equal(await runExec(["--json", "--timeout", "1", "go"], f.deps), 1);
    const envelope = JSON.parse(f.out().trim()) as ExecEnvelope;
    assert.equal(envelope.ok, false);
    assert.equal(envelope.status, "error");
    assert.equal(envelope.error, "provider 500");
  } finally {
    f.close();
  }
});

// ---- the prompt ---------------------------------------------------------------------

test("the prompt comes from stdin when the positional is `-`", async () => {
  const f = fixture({ turn: instantTurn("ok"), stdin: "  from a pipe  \n", isTerminal: true });
  try {
    assert.equal(await runExec(["--timeout", "1", "-"], f.deps), 0);
    const session = f.ctx.db.listSessions()[0];
    assert.equal(f.ctx.db.messagesFor(session.id)[0].parts[0].type, "text");
    assert.deepEqual(f.ctx.db.messagesFor(session.id)[0].parts[0], {
      type: "text",
      text: "from a pipe",
    });
  } finally {
    f.close();
  }
});

test("the prompt comes from stdin when it is absent and stdin is piped", async () => {
  const f = fixture({ turn: instantTurn("ok"), stdin: "piped prompt", isTerminal: false });
  try {
    assert.equal(await runExec(["--timeout", "1"], f.deps), 0);
    const session = f.ctx.db.listSessions()[0];
    assert.deepEqual(f.ctx.db.messagesFor(session.id)[0].parts[0], {
      type: "text",
      text: "piped prompt",
    });
  } finally {
    f.close();
  }
});

// ---- session shape --------------------------------------------------------------------

test("-w and -m land on the created session; the default workspace is the cwd", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough-exec-"));
  const f = fixture({ turn: instantTurn("ok") });
  try {
    assert.equal(
      await runExec(["-w", dir, "-m", "openai:gpt-5", "--timeout", "1", "go"], f.deps),
      0,
    );
    const session = f.ctx.db.listSessions()[0];
    assert.equal(session.workspace, await realpath(dir));
    // `originDir` is the stable project record and mirrors the workspace at creation.
    assert.equal(session.originDir, await realpath(dir));
    assert.equal(session.model, "openai:gpt-5");
    assert.equal(session.kind, "root");
    assert.match(session.title, /^exec: go$/);
  } finally {
    f.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("--port beats BOUGH_PORT, which beats the built-in default", async () => {
  // Observed through the only thing that varies with the port: the URL the client
  // reports it could not reach.
  for (
    const [argv, env, expected] of [
      [["--port", "4500", "go"], { BOUGH_PORT: "4600" }, "4500"],
      [["go"], { BOUGH_PORT: "4600" }, "4600"],
      [["go"], {}, "4321"],
    ] as const
  ) {
    const f = fixture({ env: { ...env } });
    try {
      const code = await runExec(argv, {
        ...f.deps,
        fetchFn: () => Promise.reject(new TypeError("nope")),
      });
      assert.equal(code, 2);
      assert.match(f.err(), new RegExp(`:${expected}`));
    } finally {
      f.close();
    }
  }
});

// ---- pure parsing -----------------------------------------------------------------------

test("parseExecArgs: the flag set, in both spellings", () => {
  const parsed = parseExecArgs([
    "-w",
    "/w",
    "--model=m",
    "--json",
    "--timeout",
    "30",
    "--port=4400",
    "the prompt",
  ]);
  assert.ok(!isUsageError(parsed) && !isHelpRequest(parsed));
  assert.deepEqual(parsed, {
    prompt: "the prompt",
    workspace: "/w",
    model: "m",
    json: true,
    timeoutMs: 30_000,
    port: 4400,
  });
});

test("parseExecArgs: defaults", () => {
  const parsed = parseExecArgs(["hi"]);
  assert.ok(!isUsageError(parsed) && !isHelpRequest(parsed));
  assert.equal(parsed.json, false);
  assert.equal(parsed.timeoutMs, 900_000);
  assert.equal(parsed.port, undefined);
  assert.equal(parsed.workspace, undefined);
});

test("parseExecArgs: `-` is the stdin sentinel, not a flag", () => {
  const parsed = parseExecArgs(["-"]);
  assert.ok(!isUsageError(parsed) && !isHelpRequest(parsed));
  assert.equal(parsed.prompt, "-");
});

test("parseExecArgs: `--` ends flag parsing, so a prompt may start with a dash", () => {
  const parsed = parseExecArgs(["--json", "--", "--not-a-flag"]);
  assert.ok(!isUsageError(parsed) && !isHelpRequest(parsed));
  assert.equal(parsed.prompt, "--not-a-flag");
  assert.equal(parsed.json, true);
});

test("parseExecArgs: a value flag may take a dash-leading value", () => {
  const parsed = parseExecArgs(["-m", "-weird-model", "go"]);
  assert.ok(!isUsageError(parsed) && !isHelpRequest(parsed));
  assert.equal(parsed.model, "-weird-model");
  assert.equal(parsed.prompt, "go");
});

test("parseExecArgs: a forgotten pair of quotes is an error, not a one-word prompt", () => {
  const parsed = parseExecArgs(["write", "the", "tests"]);
  assert.ok(isUsageError(parsed));
  assert.match(parsed.usageError, /quote it as a single string/);
});

test("parseExecArgs: rejects the malformed rest", () => {
  for (
    const [argv, pattern] of [
      [["--nope"], /unknown flag --nope/],
      [["-q", "x"], /unknown flag -q/],
      [["--json=1", "go"], /--json takes no value/],
      [["--timeout"], /--timeout needs a value/],
      [["--timeout", "0", "go"], /positive number of seconds/],
      [["--timeout", "abc", "go"], /positive number of seconds/],
      [["--port", "0", "go"], /wants a port number/],
      [["--port", "99999", "go"], /wants a port number/],
      [["--port", "x", "go"], /wants a port number/],
    ] as const
  ) {
    const parsed = parseExecArgs(argv);
    assert.ok(isUsageError(parsed), argv.join(" "));
    assert.match(parsed.usageError, pattern, argv.join(" "));
  }
});

// ---- the SSE reader ---------------------------------------------------------------------

test("createSseReader: a frame split across chunks is not read until it is whole", () => {
  const feed = createSseReader();
  assert.deepEqual(feed("event: turn.fin"), []);
  assert.deepEqual(feed('ished\ndata: {"a":1}'), []);
  assert.deepEqual(feed("\n\n"), [{ name: "turn.finished", data: { a: 1 } }]);
});

test("createSseReader: field order does not matter, and comments carry nothing", () => {
  const feed = createSseReader();
  const frames = feed(': connected\n\ndata: {"a":1}\nevent: message.delta\n\n: ping\n\n');
  assert.deepEqual(frames, [{ name: "message.delta", data: { a: 1 } }]);
});

test("createSseReader: several frames in one chunk, and a malformed one is dropped", () => {
  const feed = createSseReader();
  const frames = feed(
    'event: a\ndata: 1\n\nevent: b\ndata: {oops\n\nevent: c\ndata: {"ok":true}\n\n',
  );
  assert.deepEqual(frames, [
    { name: "a", data: 1 },
    { name: "c", data: { ok: true } },
  ]);
});

test("createSseReader: CRLF framing parses the same as LF", () => {
  const feed = createSseReader();
  assert.deepEqual(feed("event: x\r\ndata: 2\r\n\r\n"), [{ name: "x", data: 2 }]);
});

// ---- retry ---------------------------------------------------------------------------------

test("a retry announces itself on stderr and drops the false start from the envelope", async () => {
  const f = fixture({
    turn: (ctx, session) => {
      const messageId = crypto.randomUUID();
      ctx.bus.publish({
        type: "message.delta",
        sessionId: session.id,
        data: { messageId, delta: "half an ans" },
      });
      ctx.bus.publish({
        type: "message.retry",
        sessionId: session.id,
        data: { messageId, attempt: 2, reason: "tool input truncated mid-stream" },
      });
      ctx.bus.publish({
        type: "message.delta",
        sessionId: session.id,
        data: { messageId, delta: "the real answer" },
      });
      ctx.bus.publish({
        type: "turn.finished",
        sessionId: session.id,
        data: { turnId: crypto.randomUUID(), sessionId: session.id, status: "done" },
      });
    },
  });
  try {
    assert.equal(await runExec(["--json", "--timeout", "1", "go"], f.deps), 0);
    const envelope = JSON.parse(f.out().trim()) as ExecEnvelope;
    assert.equal(envelope.text, "the real answer");
    assert.match(f.err(), /\[retry 2: tool input truncated mid-stream\]/);
  } finally {
    f.close();
  }
});

test("--help is answered, not rejected", () => {
  // It used to fall through to "unknown flag --help" and exit 2, so the first
  // thing anyone types at a new CLI printed an error and failed a shell `&&`.
  for (const argv of [["--help"], ["-h"], ["-w", "/tmp", "--help"]]) {
    const parsed = parseExecArgs(argv);
    assert.ok(isHelpRequest(parsed), `${argv.join(" ")} should be a help request`);
  }
  // A prompt that merely CONTAINS the word is still a prompt.
  const prompt = parseExecArgs(["help me refactor this"]);
  assert.ok(!isHelpRequest(prompt));
});

test("the usage text names every flag it accepts, and the no-sandbox posture", () => {
  for (const flag of ["--workspace", "--model", "--json", "--timeout", "--port", "--help"]) {
    assert.ok(USAGE.includes(flag), `usage does not document ${flag}`);
  }
  // Spec §2. A headless client is exactly where this is easiest to forget.
  assert.match(USAGE, /no sandbox/);
});
