/**
 * Generator for `ts_journal.db` — the cross-engine compatibility fixture.
 *
 * It runs the **TypeScript** workflow engine (`src/workflow/run.ts`, the real
 * one, with the real `wf_worker.ts` Worker) over a script that exercises every
 * shape the journal key has to survive: sequential calls, a `parallel()`
 * fan-out, a `pipeline()` (stage-major coordinates), a non-ASCII prompt (the
 * UTF-16 hash), a `{schema}` call (canonicalized JSON in the key), and one
 * failing call at the end (only successes replay). The resulting database is
 * committed as test data and re-opened by the Rust engine, which must replay
 * every answered call from it without paying for one.
 *
 * Regenerate with, from the repo root:
 *
 *   bun run bough-rs/crates/bough-core/src/workflow/testdata/gen_ts_journal.ts
 *
 * The AgentRunner is a fake — no LLM, no key, no network. Only the journal
 * matters.
 */
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { Bus } from "../../../../../../src/bus.ts";
import { openDb } from "../../../../../../src/db/db.ts";
import { type AgentCall, startWorkflow, type WorkflowCtx } from "../../../../../../src/workflow/run.ts";
import type { WorkflowRun } from "../../../../../../src/schema/parts.ts";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "ts_journal.db");

/**
 * Every shape the key must survive. Kept in this file (not a separate .js) so
 * the fixture and the script that produced it cannot drift.
 */
const SCRIPT = `export const meta = { name: 'compat', description: 'the cross-engine journal fixture' }
phase('Survey')
const one = await agent('audit the handlers')
const two = await agent('fix the 🐛 in parse()', { label: 'the bug' })
const wide = await parallel([
  () => agent('branch a'),
  () => agent('branch b'),
])
const staged = await pipeline(
  ['A', 'B'],
  (item) => agent('s1 ' + item),
  (prev) => agent('s2 ' + prev),
)
const typed = await agent('report findings', {
  schema: {
    type: 'object',
    properties: { ok: { type: 'boolean' }, n: { type: 'number', default: 1 } },
    required: ['ok'],
    additionalProperties: false,
  },
})
let failed = null
try { await agent('this one fails') } catch (e) { failed = String(e.message) }
return { one, two, wide, staged, typed, failed }
`;

/** Deterministic answers, so the fixture is reproducible byte for byte. */
const runner = (call: AgentCall): Promise<string> => {
  if (call.prompt === "this one fails") {
    return Promise.reject(new Error('workflow agent "this one fails" error: boom'));
  }
  if (call.prompt === "report findings") return Promise.resolve('{"ok":true,"n":1}');
  return Promise.resolve(`report: ${call.prompt}`);
};

function completion(bus: Bus, ms = 30_000): Promise<WorkflowRun> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      off();
      reject(new Error(`the workflow did not finish within ${ms}ms`));
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

rmSync(OUT, { force: true });
const home = mkdtempSync(join(tmpdir(), "bough-compat-"));
process.env["BOUGH_HOME"] = home;

const db = openDb(OUT);
const bus = new Bus();
const session = db.createSession({
  id: "compat-session",
  title: "the cross-engine fixture",
  kind: "root",
  createdAt: 1_000,
  parentId: null,
  workspace: "/tmp/checkout",
  originDir: "/tmp/checkout",
});

const ctx: WorkflowCtx = { db, bus, runner };
const done = completion(bus);
const run = await startWorkflow(ctx, {
  sessionId: session.id,
  script: SCRIPT,
  meta: { name: "compat", description: "the cross-engine journal fixture" },
  concurrency: 4,
});
const finished = await done;
const rows = db.listWorkflowAgents(run.id);
console.log(`run ${run.id} → ${finished.status}, ${rows.length} journal rows`);
for (const row of rows) console.log(`  ${row.idx} ${row.key} ${row.status} ${row.label}`);
db.close();
rmSync(home, { recursive: true, force: true });
