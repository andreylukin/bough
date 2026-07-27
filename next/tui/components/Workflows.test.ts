/**
 * Tests for the workflow run view.
 *
 * The load-bearing one is the first: **a run's replay accounting is on screen.** Spec
 * §8 makes it a requirement of the system — "Any operation that replays returns how
 * many calls were served from the journal and how many ran live… A rerun that silently
 * replayed nothing looks exactly like a successful rerun, so the count is the only
 * thing that makes a key defect visible." The server computes it; the way that
 * requirement dies in practice is a client that receives the numbers and renders
 * something else. So the fixture is a relaunch with MIXED statuses including `cached`,
 * and the assertions are on the literal text of the header rows.
 *
 * `replay.line` in the fixture is produced by the real `replayLine` from
 * `workflow/report.ts` rather than typed out here, so a change to the sentence every
 * client is supposed to say in unison cannot pass this file by agreeing with a copy of
 * itself.
 *
 * The clock is injected everywhere (`now`), so no assertion depends on when the suite
 * runs — an elapsed-time regression would otherwise show up as a flake.
 */
import assert from "node:assert/strict";
import type { WorkflowRun } from "../../schema/parts.ts";
import type { WorkflowAgentView } from "../../workflow/control.ts";
import type { LargeRunFlag, ReplaySummary, RunCost } from "../../workflow/report.ts";
import { replayLine } from "../../workflow/report.ts";
import type { WorkflowDetail } from "../api.ts";
import {
  agentDetailRows,
  agentRows,
  footer,
  linesOf,
  phaseGroups,
  replayRows,
  runHeaderRows,
  scriptRows,
  steerActions,
  visibleAgents,
  wfGlyph,
} from "./Workflows.tsx";

const T0 = 1_700_000_000_000;
const NOW = T0 + 90_000;

function agent(over: Partial<WorkflowAgentView> & { id: string }): WorkflowAgentView {
  return {
    runId: "run-2",
    idx: 0,
    key: `k-${over.id}`,
    label: over.id,
    phase: "Review",
    prompt: "Review src/server/app.ts",
    model: "sonnet",
    status: "done",
    result: "no findings",
    error: null,
    sessionId: `sess-${over.id}`,
    startedAt: T0,
    finishedAt: T0 + 20_000,
    tokens: 1200,
    toolCalls: 3,
    activity: [],
    live: false,
    ...over,
  };
}

/** Two replayed, one done, one failed, one running, one queued — every bucket. */
const AGENTS: WorkflowAgentView[] = [
  agent({ id: "a", idx: 0, status: "cached", sessionId: null, tokens: 0, result: "cached ok" }),
  agent({ id: "b", idx: 1, status: "cached", sessionId: null, tokens: 0, result: "cached ok" }),
  agent({ id: "c", idx: 2, status: "done" }),
  agent({ id: "d", idx: 3, status: "error", result: null, error: "patch conflict in app.ts" }),
  agent({
    id: "e",
    idx: 4,
    status: "running",
    phase: "Verify",
    finishedAt: null,
    result: null,
    live: true,
  }),
  agent({
    id: "f",
    idx: 5,
    status: "queued",
    phase: "Verify",
    finishedAt: null,
    result: null,
    sessionId: null,
    tokens: 0,
  }),
];

const RUN: WorkflowRun = {
  id: "run-2",
  sessionId: "sess-owner",
  name: "audit-handlers",
  description: "Review every handler for missing error paths",
  script: "export const meta = { name: 'audit-handlers' }\nphase('Review')\n",
  phases: [{ title: "Review" }, { title: "Verify" }, { title: "Report" }],
  status: "running",
  currentPhase: "Verify",
  result: null,
  error: null,
  args: null,
  resumeOf: "run-1",
  createdAt: T0,
  finishedAt: null,
};

function summary(over: Partial<Omit<ReplaySummary, "line">> = {}): ReplaySummary {
  const base: Omit<ReplaySummary, "line"> = {
    runId: "run-2",
    sourceId: "run-1",
    replayed: 2,
    ranLive: 2,
    total: 6,
    pending: 2,
    succeeded: 1,
    failed: 1,
    stopped: 0,
    available: 5,
    final: false,
    livePrompts: ["Review src/server/app.ts"],
    ...over,
  };
  return { ...base, line: replayLine(base) };
}

const COST: RunCost = {
  runId: "run-2",
  agents: 6,
  replayed: 2,
  tokens: 4800,
  agentMs: 80_000,
  wallMs: 90_000,
  byPhase: [
    { phase: "Review", agents: 4, replayed: 2, tokens: 3600, elapsedMs: 60_000 },
    { phase: "Verify", agents: 2, replayed: 0, tokens: 1200, elapsedMs: 20_000 },
  ],
  byAgent: [],
};

function detail(over: Partial<WorkflowDetail> = {}): WorkflowDetail {
  return {
    workflow: RUN,
    agents: AGENTS,
    scriptFile: "/home/u/.bough/workflows/run-2.js",
    live: true,
    replay: summary(),
    cost: COST,
    warning: null,
    guideline: "medium",
    ...over,
  };
}

const text = (rows: ReturnType<typeof runHeaderRows>) => linesOf(rows).join("\n");

// ---- the replay summary is always on screen ---------------------------------

Deno.test("the run header reports the replay counts for a mixed-status run", () => {
  const out = text(runHeaderRows(detail(), { now: NOW }));

  // Every bucket, named. `replayed + ranLive + pending === total` is the arithmetic
  // the numbers are only safe to read as money if it holds (workflow/report.ts).
  assert.match(out, /2 replayed/);
  assert.match(out, /2 ran live/);
  assert.match(out, /2 still going/);
  assert.match(out, /of 6/);
  // `available` is the half that names the defect rather than the symptom: it is what
  // makes a later `0 replayed` legible as key drift instead of an ordinary first run.
  assert.match(out, /5 available to replay/);
  // The server's canonical sentence, verbatim, so this client says what every other
  // one says about the same run.
  assert.ok(out.includes(summary().line), `expected the wire line in:\n${out}`);
});

Deno.test("a run that replayed NOTHING of an available journal is called out", () => {
  const broken = summary({ replayed: 0, ranLive: 4, pending: 2, available: 12, succeeded: 4 });
  const rows = replayRows(broken);
  const out = linesOf(rows).join("\n");

  assert.match(out, /0 replayed/);
  assert.match(out, /12 available to replay/);
  assert.match(out, /replayed NOTHING/);
  // Tone, not just words: the counts row is the alarm, so a reader who skims colour
  // stops on it. A quiet render of these numbers is the failure this file exists for.
  assert.equal(rows[0][1].tone, "error");
  assert.equal(rows[1][0].tone, "error");
});

Deno.test("an ordinary first run reports its counts without crying wolf", () => {
  const first = summary({
    sourceId: null,
    replayed: 0,
    ranLive: 3,
    pending: 0,
    total: 3,
    available: 0,
    succeeded: 3,
    failed: 0,
  });
  const rows = replayRows(first);
  const out = linesOf(rows).join("\n");

  assert.match(out, /0 replayed · 3 ran live · of 3/);
  assert.doesNotMatch(out, /available/);
  assert.doesNotMatch(out, /NOTHING/);
  assert.notEqual(rows[0][1].tone, "error");
});

// ---- statuses, including cached ---------------------------------------------

Deno.test("cached is its own glyph, distinct from done", () => {
  assert.equal(wfGlyph("cached").glyph, "≡");
  assert.equal(wfGlyph("done").glyph, "✓");
  assert.notEqual(wfGlyph("cached").glyph, wfGlyph("done").glyph);
  assert.equal(wfGlyph("queued").glyph, "◦");
  assert.equal(wfGlyph("running").glyph, "◐");
  assert.equal(wfGlyph("error").glyph, "✗");
});

Deno.test("the agent list renders every status, and a queued agent shows no clock", () => {
  const out = linesOf(agentRows(AGENTS, 0, true, false, NOW));
  assert.equal(out.length, 6);
  assert.ok(out[0].startsWith("❯ ≡ a"), out[0]);
  assert.ok(out[2].includes("✓ c"), out[2]);
  assert.ok(out[3].includes("✗ d"), out[3]);
  // A running agent shows its live clock — the glyph already says "running", and the
  // number is what tells you it is wedged rather than slow.
  assert.match(out[4], /◐ e.*1m30s/);
  assert.match(out[5], /◦ f.*queued/);
  // A replayed call spent nothing, and the row says so by carrying no token chip.
  assert.doesNotMatch(out[0], /tok/);
  assert.match(out[2], /1\.2k tok/);
});

Deno.test("a cached agent's detail says the answer came from the journal", () => {
  const out = linesOf(agentDetailRows(AGENTS[0], false, NOW)).join("\n");
  assert.match(out, /replayed from the source run's journal — no agent ran/);
  assert.match(out, /no session — this call was replayed from the journal/);
});

Deno.test("a failed agent's detail leads with the error, not an empty outcome", () => {
  const out = linesOf(agentDetailRows(AGENTS[3], false, NOW)).join("\n");
  assert.match(out, /^✗ error/m);
  assert.match(out, /patch conflict in app\.ts/);
});

Deno.test("drill-in names the backing session so an agent is reachable", () => {
  const out = linesOf(agentDetailRows(AGENTS[2], false, NOW)).join("\n");
  assert.match(out, /session sess-c — o opens it/);
});

// ---- phases -----------------------------------------------------------------

Deno.test("declared phases appear before any agent reaches them", () => {
  const groups = phaseGroups(RUN, AGENTS);
  assert.deepEqual(groups.map((g) => g.title), ["Review", "Verify", "Report"]);
  assert.equal(groups[2].agents.length, 0); // the shape of the run, before it gets there
  assert.equal(groups[0].agents.length, 4);
});

Deno.test("the done filter folds in journal replays", () => {
  assert.equal(visibleAgents(AGENTS, "done").length, 3); // 1 done + 2 cached
  assert.equal(visibleAgents(AGENTS, "error").length, 1);
  assert.equal(visibleAgents(AGENTS, null).length, 6);
});

// ---- cost and the advisory flag ---------------------------------------------

Deno.test("cost is in the header while the run is going, per phase", () => {
  const out = text(runHeaderRows(detail(), { now: NOW }));
  assert.match(out, /4\.8k tok/);
  assert.match(out, /Review 3\.6k tok/);
  assert.match(out, /Verify 1\.2k tok/);
});

Deno.test("a large-run warning names the control that stops it, and stays advisory", () => {
  const warning: LargeRunFlag = {
    flagged: true,
    advisory: true,
    guideline: "medium",
    target: 15,
    scheduled: 40,
    tokens: 4800,
    projectedTokens: 2_000_000,
    tokenThreshold: 1_000_000,
    reasons: ["40 agents scheduled, past the medium guideline of 15"],
    stop: "POST /workflows/run-2/stop",
  };
  const out = text(runHeaderRows(detail({ warning }), { now: NOW }));
  assert.match(out, /40 agents scheduled/);
  assert.match(out, /advisory — nothing is throttled; x stops the run/);
});

// ---- steering ---------------------------------------------------------------

Deno.test("a running run offers pause before stop; a paused one offers resume", () => {
  const running = steerActions("running", true).map((a) => a.key);
  assert.deepEqual(running, ["p", "x"]);
  assert.match(steerActions("running", true)[0].label, /pause/);
  // Pausing preserves the most work: a dispatched agent allowed to finish is journaled
  // and replays; one killed in flight is not (spec §8).
  assert.match(steerActions("running", true)[0].label, /finishes in-flight agents/);

  const paused = steerActions("paused", true).map((a) => a.key);
  assert.deepEqual(paused, ["p", "x", "e"]);
  assert.match(steerActions("paused", true)[0].label, /resume/);
});

Deno.test("a finished run offers the edit-and-relaunch half of the steering loop", () => {
  const done = steerActions("done", false);
  assert.deepEqual(done.map((a) => a.key), ["r", "e"]);
  assert.match(done[1].label, /edit script & relaunch/);
});

Deno.test("a run orphaned by a restart is not offered a pause it cannot honor", () => {
  const orphaned = steerActions("running", false).map((a) => a.key);
  assert.ok(!orphaned.includes("p"), "a run this process does not hold cannot be paused");
  assert.match(steerActions("running", false)[0].label, /orphaned by a restart/);
});

Deno.test("the footer carries the steering keys at every level", () => {
  for (const level of [0, 1, 2, 3, 4] as const) {
    const line = footer(level, detail());
    assert.match(line, /p pause/, `level ${level}: ${line}`);
    assert.match(line, /x stop/, `level ${level}: ${line}`);
  }
});

// ---- the script, which is what steering edits -------------------------------

Deno.test("the script view names the mirror path — the file the loop edits", () => {
  const out = linesOf(scriptRows(detail({ live: false }))).join("\n");
  assert.match(out, /\/home\/u\/\.bough\/workflows\/run-2\.js/);
  assert.match(out, /R relaunches a NEW run from this one's journal/);
  assert.match(out, /6 calls\s+journaled here/);
  assert.match(out, /1 export const meta/);
});

Deno.test("a live run is told to pause and stop before editing", () => {
  const out = linesOf(scriptRows(detail({ live: true }))).join("\n");
  assert.match(out, /pause, then stop, before you edit/);
  assert.doesNotMatch(out, /R relaunches/);
});

// ---- the header's identity fields -------------------------------------------

Deno.test("the header says it is a relaunch, and of what", () => {
  const out = text(runHeaderRows(detail(), { now: NOW }));
  assert.match(out, /audit-handlers/);
  assert.match(out, /relaunch of run-1/);
  assert.match(out, /3\/6 agents · 1 failed/); // done + cached counted as settled
  assert.match(out, /1m30s/);
});

Deno.test("a run the server restarted under is not drawn as a working one", () => {
  const out = text(runHeaderRows(detail({ live: false }), { now: NOW }));
  assert.match(out, /\(not live here\)/);
});
