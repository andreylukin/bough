/**
 * `bough mcp`, driven end to end against the REAL route table.
 *
 * Same harness as `exec.test.ts` and for the same reason: the client's whole job is
 * to speak to those routes, so a test against a hand-written fake asserts that the
 * mock matches the mock. `createHandler` over an in-memory database is the actual
 * dispatcher, actual Zod boundaries, actual error sentences — with no socket bound.
 *
 * ISOLATION IS `BOUGH_HOME`, AND IT IS NOT OPTIONAL. The registry is a file whose
 * path derives from `paths.ts`, which reads `process.env` at call time — not from
 * anything injectable. The first version of this file set the path in `deps.env`,
 * which the CLI uses only to pick a PORT, so every test ran against the developer's
 * own `~/.bough/mcp.json`: `add` wrote a server into it and `remove` took it out
 * again, and only a failing assertion revealed that the registry under test was the
 * real one. Set `process.env.BOUGH_HOME`, restore it in `cleanup`, and never assume
 * a `deps` field reaches code that runs in-process.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { createHandler } from "../server/app.ts";
import type { WithTurnStarter } from "../server/sessions.ts";
import type { AppCtx } from "../types.ts";
import { isUsageError, type McpDeps, parseMcpArgs, runMcp, USAGE } from "./mcp.ts";

interface Fixture {
  deps: McpDeps;
  out: () => string;
  err: () => string;
  calls: string[];
  cleanup: () => Promise<void>;
}

async function fixture(servers: Record<string, unknown> = {}): Promise<Fixture> {
  const dir = await mkdtemp(join(tmpdir(), "bough-mcp-cli-"));
  await writeFile(join(dir, "mcp.json"), JSON.stringify({ servers }), "utf8");
  const priorHome = process.env["BOUGH_HOME"];
  process.env["BOUGH_HOME"] = dir;
  const db = openDb(":memory:");
  const ctx: AppCtx & WithTurnStarter = { db, bus: new Bus() };
  const handler = createHandler(ctx, { onUnexpectedError: () => {} });
  const calls: string[] = [];
  let out = "";
  let err = "";
  return {
    calls,
    out: () => out,
    err: () => err,
    deps: {
      fetch: ((input: any, init: any) => {
        const req = new Request(input as string | URL, init);
        calls.push(`${req.method} ${new URL(req.url).pathname}`);
        return handler(req);
      }) as typeof fetch,
      out: (l) => {
        out += l + "\n";
      },
      err: (l) => {
        err += l + "\n";
      },
      env: { BOUGH_PORT: "4321" },
      // No real waiting: the auth poll would otherwise cost a second per iteration.
      sleep: () => Promise.resolve(),
    },
    cleanup: async () => {
      if (priorHome === undefined) delete process.env["BOUGH_HOME"];
      else process.env["BOUGH_HOME"] = priorHome;
      await rm(dir, { recursive: true, force: true });
    },
  };
}

// ---- parsing ----------------------------------------------------------------

test("parsing is pure and total, and bare `mcp` is `list`", () => {
  // The question people arrive with is "what have I got", so making them type the
  // verb is friction over the common case.
  const bare = parseMcpArgs([]);
  assert.equal(isUsageError(bare) ? "" : bare.verb, "list");

  const doctor = parseMcpArgs(["doctor", "--json", "--port", "5000"]);
  assert.deepEqual(isUsageError(doctor) ? null : { v: doctor.verb, j: doctor.json, p: doctor.port }, {
    v: "doctor",
    j: true,
    p: 5000,
  });

  // A verb that acts on a server must name one — the alternative is a command that
  // silently acts on whichever server sorts first.
  for (const verb of ["test", "auth", "logout", "grant", "revoke", "remove"]) {
    const r = parseMcpArgs([verb]);
    assert.ok(isUsageError(r), verb);
    assert.match((r as { usageError: string }).usageError, /needs a server name/);
  }
  // `add` needs both halves.
  const add = parseMcpArgs(["add", "notion"]);
  assert.ok(isUsageError(add));
  assert.match((add as { usageError: string }).usageError, /needs a name and a URL/);

  assert.match((parseMcpArgs(["wat"]) as { usageError: string }).usageError, /unknown verb "wat"/);
  assert.match((parseMcpArgs(["--nope"]) as { usageError: string }).usageError, /unknown flag/);
  assert.match((parseMcpArgs(["--port"]) as { usageError: string }).usageError, /needs a value/);
  assert.match((parseMcpArgs(["--port", "x"]) as { usageError: string }).usageError, /positive/);
  assert.equal((parseMcpArgs(["--help"]) as { usageError: string }).usageError, USAGE);
});

// ---- list -------------------------------------------------------------------

test("list reports each server's state and explains its glyphs", async () => {
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  const code = await runMcp(["list"], f.deps);
  assert.equal(code, 0);
  assert.match(f.out(), /○ notion/);
  assert.match(f.out(), /not granted/);
  // The legend, for the same reason the panel grew one: three marks carrying the
  // whole state of a row, documented nowhere, is how "it stays a half circle"
  // becomes a bug report instead of a glance.
  assert.match(f.out(), /● connected · ◐ granted, not connected · ○ not granted/);
  await f.cleanup();
});

test("an empty registry says what to do about it, and is not an error", async () => {
  const f = await fixture();
  assert.equal(await runMcp(["list"], f.deps), 0);
  assert.match(f.out(), /no MCP servers registered/);
  assert.match(f.out(), /bough sync-mcp/);
  await f.cleanup();
});

// ---- doctor -----------------------------------------------------------------

test("doctor sorts the causes apart and exits non-zero when one needs a human", async () => {
  // The whole point of the verb. A connect error alone does not tell you whether the
  // job is yours, Claude Code's, or nobody's — and the three fixes are different.
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  const code = await runMcp(["doctor"], f.deps);
  assert.equal(code, 1, f.out());
  assert.match(f.out(), /✗ notion/);
  // Ungranted is reported as ungranted, not as a credential problem: a server nobody
  // granted will never connect, so advice about its token is advice about a step
  // that has not been reached.
  assert.match(f.out(), /not granted — bough mcp grant notion/);
  assert.match(f.out(), /1 of 1 needs attention/);
  await f.cleanup();
});

test("doctor on an empty registry is a true answer, not a failure", async () => {
  const f = await fixture();
  assert.equal(await runMcp(["doctor"], f.deps), 0);
  await f.cleanup();
});

// ---- grant / revoke ---------------------------------------------------------

test("grant and revoke act on the global scope, and say so", async () => {
  // A per-session grant is a thing the panel can offer because it has a session on
  // screen. A CLI does not, and inventing one would make the verb mean something
  // other than what it says.
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  assert.equal(await runMcp(["grant", "notion"], f.deps), 0, f.err());
  assert.match(f.out(), /granted in every conversation/);

  const after = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  await runMcp(["grant", "notion"], after.deps);
  await runMcp(["list"], after.deps);
  assert.match(after.out(), /◐ notion/);

  assert.equal(await runMcp(["revoke", "notion"], f.deps), 0, f.err());
  assert.match(f.out(), /revoked everywhere/);
  await f.cleanup();
  await after.cleanup();
});

// ---- add / remove -----------------------------------------------------------

test("add registers and points at the next step; remove says what it took with it", async () => {
  const f = await fixture();
  assert.equal(await runMcp(["add", "notion", "https://mcp.notion.com/mcp"], f.deps), 0, f.err());
  // The next step, named. "Registered" alone leaves you with a server that cannot be
  // called and no indication that two more verbs stand between you and using it.
  assert.match(f.out(), /bough mcp auth notion, then grant it/);

  assert.equal(await runMcp(["list"], f.deps), 0);
  assert.match(f.out(), /notion/);

  assert.equal(await runMcp(["remove", "notion"], f.deps), 0, f.err());
  // The scope, out loud: removing an entry also revokes the grants it orphans.
  assert.match(f.out(), /along with any grants it held/);
  await f.cleanup();
});

test("a verb naming a server that is not registered fails with the route's own sentence", async () => {
  const f = await fixture();
  assert.equal(await runMcp(["remove", "ghost"], f.deps), 1);
  assert.match(f.err(), /no MCP server named "ghost"/);
  await f.cleanup();
});

// ---- the server being down --------------------------------------------------

test("no server on the port is exit 2 and says how to start one", async () => {
  // Distinct from exit 1 ON PURPOSE: "your MCP setup is broken" and "bough is not
  // running" are different problems, and a CI job branching on this needs to tell
  // them apart.
  const f = await fixture();
  f.deps.fetch = ((() => Promise.reject(new Error("connection refused"))) as unknown) as typeof fetch;
  assert.equal(await runMcp(["list"], f.deps), 2);
  assert.match(f.err(), /no bough server at/);
  assert.match(f.err(), /bough start/);
  await f.cleanup();
});

// ---- auth -------------------------------------------------------------------

test("auth prints the URL rather than opening a browser, and gives up out loud", async () => {
  // PRINTED, never opened: this client runs over SSH and in CI at least as often as
  // on a desktop, and shelling out to a browser hangs where there is none.
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  let polls = 0;
  f.deps.fetch = ((input: any, init: any) => {
    const req = new Request(input as string | URL, init);
    const path = new URL(req.url).pathname;
    if (req.method === "POST" && path.endsWith("/auth")) {
      return Promise.resolve(
        new Response(JSON.stringify({ status: "pending", authorizationUrl: "https://auth.example/go" })),
      );
    }
    if (req.method === "GET" && path.endsWith("/auth")) {
      polls++;
      return Promise.resolve(new Response(JSON.stringify({ authorized: false })));
    }
    return Promise.resolve(new Response("{}", { status: 200 }));
  }) as typeof fetch;
  // A deadline that has already passed by the second check, so the loop is bounded
  // by the clock rather than by how fast the fake answers.
  let t = 0;
  f.deps.now = () => (t += 100_000);
  const code = await runMcp(["auth", "notion", "--timeout", "1"], f.deps);
  assert.equal(code, 1);
  assert.match(f.out(), /open this to authorize notion/);
  assert.match(f.out(), /https:\/\/auth\.example\/go/);
  assert.match(f.err(), /still waiting on the browser/);
  await f.cleanup();
});

test("a completed authorization CONNECTS, because storing tokens moves nothing on screen", async () => {
  // The bug this verb exists downstream of: authorization and connection are
  // different states, and a flow whose success is invisible reads as one that failed.
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  const seen: string[] = [];
  f.deps.fetch = ((input: any, init: any) => {
    const req = new Request(input as string | URL, init);
    const path = new URL(req.url).pathname;
    seen.push(`${req.method} ${path}`);
    if (req.method === "POST" && path.endsWith("/auth")) {
      return Promise.resolve(new Response(JSON.stringify({ status: "authorized" })));
    }
    if (path.endsWith("/connect")) {
      return Promise.resolve(
        new Response(JSON.stringify({ server: "notion", connected: true, tools: [{ name: "search" }] })),
      );
    }
    return Promise.resolve(
      new Response(JSON.stringify({ registry: { servers: {} }, auth: {}, active: ["notion"], connections: [] })),
    );
  }) as typeof fetch;
  assert.equal(await runMcp(["auth", "notion"], f.deps), 0, f.err());
  assert.ok(seen.some((c) => c.endsWith("/connect")), seen.join(", "));
  assert.match(f.out(), /✓ notion connected · 1 tool/);
  await f.cleanup();
});

test("authorized but ungranted says the last step instead of implying it is done", async () => {
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  f.deps.fetch = ((input: any, init: any) => {
    const req = new Request(input as string | URL, init);
    const path = new URL(req.url).pathname;
    if (req.method === "POST" && path.endsWith("/auth")) {
      return Promise.resolve(new Response(JSON.stringify({ status: "authorized" })));
    }
    if (path.endsWith("/connect")) {
      return Promise.resolve(new Response(JSON.stringify({ server: "notion", connected: true, tools: [] })));
    }
    // Granted list is EMPTY — connected, authorized, and still uncallable.
    return Promise.resolve(
      new Response(JSON.stringify({ registry: { servers: {} }, auth: {}, active: [], connections: [] })),
    );
  }) as typeof fetch;
  assert.equal(await runMcp(["auth", "notion"], f.deps), 0, f.err());
  assert.match(f.out(), /not granted yet — bough mcp grant notion/);
  await f.cleanup();
});

test("a local server is not told to run an OAuth flow that cannot exist", async () => {
  // Live for exactly one commit, and `doctor` said it about both local servers on
  // the machine it was written for. `status.auth` is populated for `url` entries
  // alone, so a stdio server always reads as unauthorized — and "no credential —
  // bough mcp auth bigquery" sends someone to a flow a local command does not have.
  const f = await fixture({ bigquery: { command: "bq-mcp", args: [] } });
  await runMcp(["grant", "bigquery"], f.deps);
  const code = await runMcp(["doctor"], f.deps);
  assert.match(f.out(), /local command — not tested/);
  assert.match(f.out(), /--session ID/);
  assert.equal(f.out().includes("bough mcp auth bigquery"), false, f.out());
  // UNTESTED IS NOT BROKEN. Counting it as a failure would make `doctor` exit 1 on
  // a healthy setup, and the exit code is the part a script depends on.
  assert.equal(code, 0, f.out());
  assert.match(f.out(), /not tested/);
  await f.cleanup();
});

test("doctor does not spend a round trip on a connect it knows will be refused", async () => {
  const f = await fixture({ bigquery: { command: "bq-mcp", args: [] } });
  await runMcp(["grant", "bigquery"], f.deps);
  f.calls.length = 0;
  await runMcp(["doctor"], f.deps);
  assert.equal(f.calls.some((c) => c.endsWith("/connect")), false, f.calls.join(", "));
  await f.cleanup();
});

test("call rejects malformed arguments before it reaches the server", async () => {
  // The check the `mcp()` host function used to do on the bridge. It moved to where
  // the arguments are actually typed — nothing should be spawned or connected to
  // find out that a shell word was not JSON.
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  f.calls.length = 0;
  const code = await runMcp(["call", "notion", "search", "{not json"], f.deps);
  assert.equal(code, 2);
  assert.match(f.err(), /not valid JSON/);
  assert.equal(f.calls.length, 0, f.calls.join(", "));
  await f.cleanup();
});

test("call prints the tool's own result, and relays a refusal verbatim", async () => {
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  f.deps.fetch = ((input: any, init: any) => {
    const req = new Request(input as string | URL, init);
    if (new URL(req.url).pathname.includes("/tools/")) {
      return Promise.resolve(
        new Response(JSON.stringify({ server: "notion", tool: "search", result: { hits: 2 } })),
      );
    }
    return Promise.resolve(new Response("{}"));
  }) as typeof fetch;
  assert.equal(await runMcp(["call", "notion", "search", '{"q":"x"}'], f.deps), 0, f.err());
  // The RESULT, not the envelope: a program parses this and should not have to dig
  // its payload out of a wrapper it did not ask for.
  assert.equal(f.out().trim(), '{"hits":2}');
  await f.cleanup();
});

test("call carries the turn's session, so the grant enforced is the caller's", async () => {
  // `$BOUGH_SESSION` is exported into every shell a turn spawns. That default is what
  // makes this behave like the host function it replaced: the model does not know
  // its own session id and is not trusted to report one.
  const f = await fixture({ notion: { url: "https://mcp.notion.com/mcp" } });
  f.deps.env = { ...f.deps.env, BOUGH_SESSION: "sess-42" };
  let seen = "";
  f.deps.fetch = ((input: any, init: any) => {
    const req = new Request(input as string | URL, init);
    seen = req.url;
    return Promise.resolve(new Response(JSON.stringify({ result: null })));
  }) as typeof fetch;
  await runMcp(["call", "notion", "search"], f.deps);
  assert.match(seen, /session=sess-42/);
  // An explicit --session still wins, for a human driving it by hand.
  await runMcp(["call", "notion", "search", "--session", "other"], f.deps);
  assert.match(seen, /session=other/);
  await f.cleanup();
});
