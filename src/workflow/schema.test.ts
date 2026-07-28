/**
 * Structured agent output (T5.3). The three acceptance facts, in order:
 *
 *   1. **A valid report parses** and the call resolves with the CANONICAL JSON of
 *      the validated value — not the agent's fenced markdown — because
 *      `harness/wf_worker.ts` parses that string and `workflow/run.ts` journals it,
 *      so a replayed call and a live one have to be byte-identical to the script.
 *   2. **A mismatch retries**, with the validation errors and the rejected report
 *      fed into the next attempt's prompt. A fresh subagent has no memory of the
 *      previous try, so a bare "try again" would re-run the same task with the same
 *      information and get the same answer.
 *   3. **An exhausted retry FAILS the call** rather than resolving to a malformed
 *      object. That is the whole invariant: a thrown failure lands in a `parallel()`
 *      slot as `null` and drops a `pipeline()` item — both of which the script
 *      already handles — where a resolved-but-broken object surfaces as a
 *      `TypeError` three stages later in a detached run nobody is watching.
 *
 * Plus the submit-time gate, which is the cheap half of the same idea: an
 * unsupported schema is an authoring mistake, and it is rejected before a single
 * subagent launches rather than after forty of them have billed.
 *
 * The last two tests drive a REAL workflow worker through the real
 * engine with a fake `AgentRunner`, because the seam that matters most — canonical
 * JSON in, `JSON.parse` out, throw becomes `null` in a `parallel()` slot — spans
 * three modules and a postMessage wire, and a unit test of any one of them proves
 * none of it.
 *
 * Hermetic and offline: an in-memory database, a real bus, no network, no key, and
 * `BOUGH_HOME` pointed at a temp dir for the duration of each engine call.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`, which is not a
 * dependency of this repo.
 */
import { test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { WorkflowError } from "../errors.ts";
import type { WorkflowRun } from "../schema/parts.ts";
import {
  type AgentCall,
  type AgentRunner,
  startWorkflow,
  type StartOpts,
  type WorkflowCtx,
} from "./run.ts";
import {
  assertOutputSchema,
  checkOutputSchema,
  DEFAULT_ATTEMPTS,
  extractJson,
  MAX_ERRORS,
  schemaContract,
  structuredRunner,
  structuredWorkflowCtx,
  validateInstance,
} from "./schema.ts";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/** The shape a fan-out actually asks for: a list of typed findings. */
const FINDINGS = {
  type: "object",
  properties: {
    file: { type: "string" },
    findings: {
      type: "array",
      items: {
        type: "object",
        properties: {
          title: { type: "string" },
          severity: { type: "string", enum: ["low", "high"] },
        },
        required: ["title", "severity"],
        additionalProperties: false,
      },
    },
  },
  required: ["file", "findings"],
  additionalProperties: false,
} as const;

const GOOD = {
  file: "src/a.ts",
  findings: [{ title: "unchecked index", severity: "high" }],
};

const signal = (): AbortSignal => new AbortController().signal;
const noSpawn = () => {};

/** A runner scripted with one report per attempt, recording the prompts it saw. */
function scripted(reports: string[]): { runner: AgentRunner; prompts: string[] } {
  const prompts: string[] = [];
  const runner: AgentRunner = (call: AgentCall) => {
    prompts.push(call.prompt);
    const next = reports[prompts.length - 1];
    if (next === undefined) throw new Error(`unexpected attempt ${prompts.length}`);
    return Promise.resolve(next);
  };
  return { runner, prompts };
}

// ---------------------------------------------------------------------------
// the submit-time gate
// ---------------------------------------------------------------------------

test("a well-formed schema is accepted, including $defs, enum and anyOf unions", () => {
  assert.equal(checkOutputSchema(FINDINGS), null);
  assert.equal(
    checkOutputSchema({
      type: "object",
      $defs: {
        Note: {
          type: "object",
          properties: { body: { type: "string" } },
          required: ["body"],
          additionalProperties: false,
        },
      },
      properties: {
        note: { $ref: "#/$defs/Note" },
        // The supported way to say "string or null".
        owner: { anyOf: [{ type: "string" }, { type: "null" }] },
        kind: { enum: ["a", "b"] },
      },
      required: ["note"],
      additionalProperties: false,
    }),
    null,
  );
});

test("an object without additionalProperties: false is rejected, and says why", () => {
  const bad = checkOutputSchema({
    type: "object",
    properties: { a: { type: "string" } },
    required: ["a"],
  });
  assert.ok(bad, "a schema with an open object must not be accepted");
  assert.match(bad, /additionalProperties/);
  // Names the consequence, not just the rule (spec §6: error text is a surface).
  assert.match(bad, /invented field/);
});

test("numeric, length and regex constraints are rejected by name with the move", () => {
  for (
    const [keyword, schema] of [
      ["minItems", { findings: { type: "array", items: { type: "string" }, minItems: 3 } }],
      ["maxLength", { file: { type: "string", maxLength: 80 } }],
      ["minimum", { score: { type: "number", minimum: 0 } }],
      ["pattern", { file: { type: "string", pattern: "^src/" } }],
    ] as const
  ) {
    const bad = checkOutputSchema({
      type: "object",
      properties: schema,
      required: Object.keys(schema),
      additionalProperties: false,
    });
    assert.ok(bad, `${keyword} must be rejected`);
    assert.match(bad, new RegExp(keyword));
    assert.match(bad, /check the value in the script/);
  }
});

test("a recursive schema is rejected and the cycle is named", () => {
  const bad = checkOutputSchema({
    type: "object",
    $defs: {
      Node: {
        type: "object",
        properties: { child: { $ref: "#/$defs/Node" } },
        required: ["child"],
        additionalProperties: false,
      },
    },
    properties: { root: { $ref: "#/$defs/Node" } },
    required: ["root"],
    additionalProperties: false,
  });
  assert.ok(bad, "recursion must not be accepted");
  assert.match(bad, /recursive/);
  assert.match(bad, /Node → Node/);
});

test("the other unsupported shapes are rejected: oneOf, type arrays, itemless arrays, unknown keywords, dangling $ref", () => {
  const cases: Array<[RegExp, unknown]> = [
    [/oneOf/, { a: { oneOf: [{ type: "string" }] } }],
    [/type` array/, { a: { type: ["string", "null"] } }],
    [/items/, { a: { type: "array" } }],
    [/unknown schema keyword/, { a: { type: "string", propertys: 1 } }],
    [/does not define/, { a: { $ref: "#/$defs/Missing" } }],
  ];
  for (const [pattern, properties] of cases) {
    const bad = checkOutputSchema({
      type: "object",
      properties,
      required: ["a"],
      additionalProperties: false,
    });
    assert.ok(bad, `expected a rejection matching ${pattern}`);
    assert.match(bad, pattern);
  }
  assert.match(checkOutputSchema("nope") ?? "", /must be a JSON Schema object/);
  assert.match(
    checkOutputSchema({ type: "array", items: { type: "string" } }) ?? "",
    /root must be/,
  );
});

test("assertOutputSchema throws a 400 the script can catch", () => {
  assert.throws(
    () => assertOutputSchema({ type: "object", properties: { a: { type: "string" } } }),
    (err: unknown) => err instanceof WorkflowError && err.status === 400,
  );
  assertOutputSchema(FINDINGS); // does not throw
});

// ---------------------------------------------------------------------------
// instance validation
// ---------------------------------------------------------------------------

test("validateInstance accepts a conforming value and locates every fault by path", () => {
  assert.deepEqual(validateInstance(FINDINGS, GOOD), []);

  const errors = validateInstance(FINDINGS, {
    file: 7,
    findings: [{ title: "ok", severity: "medium" }, { severity: "low", extra: 1 }],
  });
  const joined = errors.join("\n");
  assert.match(joined, /`\/file`: expected string, got 7/);
  assert.match(joined, /`\/findings\/0\/severity`: "medium" is not one of "low", "high"/);
  assert.match(joined, /`\/findings\/1`: missing required property `title`/);
  assert.match(joined, /`\/findings\/1`: unexpected property `extra`/);
});

test("validateInstance handles anyOf unions, $ref and the error cap", () => {
  const schema = {
    type: "object",
    $defs: {
      Note: {
        type: "object",
        properties: { body: { type: "string" } },
        required: ["body"],
        additionalProperties: false,
      },
    },
    properties: {
      owner: { anyOf: [{ type: "string" }, { type: "null" }] },
      note: { $ref: "#/$defs/Note" },
    },
    required: ["owner", "note"],
    additionalProperties: false,
  };
  assert.deepEqual(validateInstance(schema, { owner: null, note: { body: "x" } }), []);
  assert.deepEqual(validateInstance(schema, { owner: "me", note: { body: "x" } }), []);
  assert.match(
    validateInstance(schema, { owner: 3, note: { body: "x" } }).join(),
    /matched none of the 2 allowed shapes/,
  );
  assert.match(
    validateInstance(schema, { owner: null, note: { body: 1 } }).join(),
    /`\/note\/body`: expected string/,
  );

  const many = {
    type: "object",
    properties: Object.fromEntries(
      Array.from({ length: 40 }, (_, i) => [`f${i}`, { type: "string" }]),
    ),
    required: Array.from({ length: 40 }, (_, i) => `f${i}`),
    additionalProperties: false,
  };
  assert.equal(validateInstance(many, {}).length, MAX_ERRORS);
});

// ---------------------------------------------------------------------------
// reading JSON out of a report
// ---------------------------------------------------------------------------

test("extractJson reads a bare body, a fenced block, and JSON buried in prose", () => {
  assert.deepEqual(extractJson(JSON.stringify(GOOD)), { ok: true, value: GOOD });
  assert.deepEqual(
    extractJson("Here is what I found.\n\n```json\n" + JSON.stringify(GOOD) + "\n```\n"),
    { ok: true, value: GOOD },
  );
  assert.deepEqual(
    extractJson(`I reviewed it. Result: ${JSON.stringify(GOOD)} — that is all.`),
    { ok: true, value: GOOD },
  );
});

test("extractJson takes the LAST complete value, and reports prose-only honestly", () => {
  // The first block is the example the agent was quoting back; the last is its answer.
  const report = '```json\n{"file":"example","findings":[]}\n```\n' +
    "Now the real one:\n```json\n" + JSON.stringify(GOOD) + "\n```";
  const found = extractJson(report);
  assert.ok(found.ok);
  assert.deepEqual(found.value, GOOD);

  assert.deepEqual(extractJson("I could not do it, sorry."), { ok: false });
  assert.deepEqual(extractJson("   "), { ok: false });
  // A brace in prose is not a JSON value.
  assert.deepEqual(extractJson("use the {foo} placeholder"), { ok: false });
});

// ---------------------------------------------------------------------------
// the runner decorator — the three acceptance facts
// ---------------------------------------------------------------------------

test("a valid report resolves to canonical JSON, and the prompt carried the contract", async () => {
  const { runner, prompts } = scripted([
    "Reviewed the file.\n\n```json\n" + JSON.stringify(GOOD, null, 2) + "\n```",
  ]);
  const out = await structuredRunner(runner)(
    { prompt: "Review src/a.ts", label: "a", schema: FINDINGS },
    signal(),
    noSpawn,
  );

  // Canonical, not the fenced markdown: the worker JSON.parses this and the
  // journal replays it verbatim.
  assert.equal(out, JSON.stringify(GOOD));
  assert.deepEqual(JSON.parse(out), GOOD);

  assert.equal(prompts.length, 1, "a valid first report must not be retried");
  assert.ok(prompts[0].startsWith("Review src/a.ts"), "the task survives verbatim");
  assert.match(prompts[0], /RETURN FORMAT/);
  assert.match(prompts[0], /"additionalProperties": false/);
});

test("a call with no schema passes through untouched", async () => {
  const { runner, prompts } = scripted(["just prose, and that is fine"]);
  const out = await structuredRunner(runner)(
    { prompt: "Summarize", label: "a" },
    signal(),
    noSpawn,
  );
  assert.equal(out, "just prose, and that is fine");
  assert.deepEqual(prompts, ["Summarize"], "no contract is appended without a schema");
});

test("a schema mismatch RETRIES, and the retry is told exactly what was wrong", async () => {
  const { runner, prompts } = scripted([
    '```json\n{"file":"src/a.ts","findings":[{"title":"unchecked index"}]}\n```',
    "```json\n" + JSON.stringify(GOOD) + "\n```",
  ]);
  const out = await structuredRunner(runner, { attempts: 3 })(
    { prompt: "Review src/a.ts", label: "a", schema: FINDINGS },
    signal(),
    noSpawn,
  );

  assert.deepEqual(JSON.parse(out), GOOD);
  assert.equal(prompts.length, 2, "exactly one retry");
  assert.match(prompts[1], /PREVIOUS ATTEMPT REJECTED/);
  assert.match(prompts[1], /missing required property `severity`/);
  assert.match(prompts[1], /unchecked index/, "the rejected report is quoted back");
  assert.ok(prompts[1].startsWith("Review src/a.ts"), "the task is still the task");
});

test("a report with no JSON at all retries too", async () => {
  const { runner, prompts } = scripted([
    "I read the file and it looks fine to me.",
    JSON.stringify(GOOD),
  ]);
  const out = await structuredRunner(runner, { attempts: 2 })(
    { prompt: "Review src/a.ts", label: "a", schema: FINDINGS },
    signal(),
    noSpawn,
  );
  assert.deepEqual(JSON.parse(out), GOOD);
  assert.match(prompts[1], /no JSON value at all/);
});

test("an exhausted retry FAILS the call rather than returning junk", async () => {
  const junk = '```json\n{"file":"src/a.ts"}\n```';
  const { runner, prompts } = scripted([junk, junk, junk]);

  let thrown: unknown;
  let resolved: string | undefined;
  try {
    resolved = await structuredRunner(runner, { attempts: 3 })(
      { prompt: "Review src/a.ts", label: "a", schema: FINDINGS },
      signal(),
      noSpawn,
    );
  } catch (err) {
    thrown = err;
  }

  assert.equal(resolved, undefined, "it must not resolve with a malformed object");
  assert.ok(thrown instanceof WorkflowError, "and it must throw so parallel() can slot null");
  assert.equal((thrown as WorkflowError).status, 422);
  const message = (thrown as Error).message;
  assert.match(message, /after 3 attempt/);
  assert.match(message, /missing required property `findings`/);
  assert.match(message, /simplify the schema/, "the error names the move, not just the fault");
  assert.equal(prompts.length, 3, "exactly the budget, no more");
});

test("an unusable schema is rejected before a single subagent launches", async () => {
  const { runner, prompts } = scripted([]);
  await assert.rejects(
    () =>
      structuredRunner(runner)(
        {
          prompt: "Review src/a.ts",
          label: "a",
          // Open object: the gate's most common catch.
          schema: { type: "object", properties: { a: { type: "string" } } },
        },
        signal(),
        noSpawn,
      ),
    (err: unknown) => err instanceof WorkflowError && err.status === 400,
  );
  assert.equal(prompts.length, 0, "nothing may bill for a schema that cannot work");
});

test("a failing subagent is not retried — that is a different failure", async () => {
  let calls = 0;
  const runner: AgentRunner = () => {
    calls++;
    return Promise.reject(new Error("the subagent was interrupted before reporting"));
  };
  await assert.rejects(
    () =>
      structuredRunner(runner, { attempts: 3 })(
        { prompt: "Review", label: "a", schema: FINDINGS },
        signal(),
        noSpawn,
      ),
    /interrupted before reporting/,
  );
  assert.equal(calls, 1, "an interrupt must propagate, not burn the retry budget");
});

test("a stopped run does not start another attempt", async () => {
  const ctrl = new AbortController();
  let calls = 0;
  const runner: AgentRunner = () => {
    calls++;
    ctrl.abort();
    return Promise.resolve("not json");
  };
  await assert.rejects(
    () =>
      structuredRunner(runner, { attempts: 3 })(
        { prompt: "Review", label: "a", schema: FINDINGS },
        ctrl.signal,
        noSpawn,
      ),
    (err: unknown) => err instanceof WorkflowError && err.status === 409,
  );
  assert.equal(calls, 1);
});

test("the contract and the attempt budget are stable facts", () => {
  assert.equal(DEFAULT_ATTEMPTS, 3);
  const contract = schemaContract(FINDINGS);
  assert.match(contract, /exactly one JSON value/);
  assert.match(contract, /"severity"/, "the schema itself travels to the agent");
});

// ---------------------------------------------------------------------------
// end to end: real worker, real engine, fake subagents
// ---------------------------------------------------------------------------

interface Harness {
  db: SqliteDb;
  bus: Bus;
  sessionId: string;
  home: string;
  close(): void;
}

function harness(): Harness {
  const db = openDb(":memory:");
  const bus = new Bus();
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "the orchestrator",
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });
  const home = mkdtempSync(join(tmpdir(), "bough-schema-"));
  return {
    db,
    bus,
    sessionId: session.id,
    home,
    close() {
      db.close();
      try {
        rmSync(home, { recursive: true, force: true });
      } catch { /* already gone */ }
    },
  };
}

async function withHome<T>(home: string, fn: () => Promise<T>): Promise<T> {
  const prior = process.env["BOUGH_HOME"];
  process.env["BOUGH_HOME"] = home;
  try {
    return await fn();
  } finally {
    if (prior === undefined) delete process.env["BOUGH_HOME"];
    else process.env["BOUGH_HOME"] = prior;
  }
}

function completion(bus: Bus, ms = 20_000): Promise<WorkflowRun> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      off();
      reject(new Error(`workflow did not finish within ${ms}ms`));
    }, ms);
    const off = bus.subscribe((e) => {
      if (e.type !== "workflow.updated") return;
      const run = e.data as WorkflowRun;
      if (run.status === "running" || run.status === "paused") return;
      clearTimeout(timer);
      off();
      resolve(run);
    });
  });
}

async function runScript(
  h: Harness,
  runner: AgentRunner,
  script: string,
  opts: Partial<StartOpts> = {},
): Promise<WorkflowRun> {
  // The boot seam, exercised: production wraps the ctx exactly like this.
  const ctx: WorkflowCtx = structuredWorkflowCtx({ db: h.db, bus: h.bus, runner }, {
    attempts: 2,
  });
  const done = completion(h.bus);
  await withHome(
    h.home,
    () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script,
        meta: { name: "test", description: "a structured-output workflow" },
        concurrency: 4,
        ...opts,
      }),
  );
  return await done;
}

const SCRIPT_SCHEMA = JSON.stringify(FINDINGS);

test("a script's agent(prompt, {schema}) receives a PARSED object", async () => {
  const h = harness();
  try {
    const runner: AgentRunner = () =>
      // Fenced and chatty, the way a real report arrives.
      Promise.resolve("Done.\n\n```json\n" + JSON.stringify(GOOD, null, 2) + "\n```");

    const run = await runScript(
      h,
      runner,
      `const SCHEMA = ${SCRIPT_SCHEMA};
       const r = await agent('Review src/a.ts', { schema: SCHEMA, label: 'a' });
       // Indexing it is the whole point: the script branches on typed data.
       return { file: r.file, first: r.findings[0].title, n: r.findings.length };`,
    );

    assert.equal(run.status, "done", run.error ?? "");
    assert.deepEqual(run.result, { file: "src/a.ts", first: "unchecked index", n: 1 });
  } finally {
    h.close();
  }
});

test("a persistently malformed report fails that agent() — parallel slots it null", async () => {
  const h = harness();
  try {
    let calls = 0;
    const runner: AgentRunner = () => {
      calls++;
      return Promise.resolve("It went fine, no issues found."); // never JSON
    };

    const run = await runScript(
      h,
      runner,
      `const SCHEMA = ${SCRIPT_SCHEMA};
       const out = await parallel([
         () => agent('Review src/a.ts', { schema: SCHEMA, label: 'a' }),
       ]);
       return out;`,
    );

    assert.equal(run.status, "done", run.error ?? "");
    assert.deepEqual(run.result, [null], "a schema failure is a failed agent, not junk data");
    assert.equal(calls, 2, "the retry budget was spent, and only that");

    const agents = h.db.listWorkflowAgents(run.id);
    assert.equal(agents.length, 1);
    assert.equal(agents[0].status, "error");
    assert.match(agents[0].error ?? "", /never matched the schema/);
    assert.equal(agents[0].result, null, "nothing malformed reaches the journal");
  } finally {
    h.close();
  }
});
