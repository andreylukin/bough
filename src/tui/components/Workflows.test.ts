// The workflows browser's pure grouping/filtering — the two functions BOTH panes
// and every selection-scoped key index through, so a disagreement between them
// means `x` acts on a row the view isn't showing.
import { assertEquals } from "jsr:@std/assert@1";
import { phaseGroups, visibleAgents } from "./Workflows.tsx";
import type { WfAgentView } from "../api.ts";
import type { WorkflowRun } from "../../db/db.ts";

function agent(label: string, phase: string | null, status: string): WfAgentView {
  return {
    id: label,
    runId: "r",
    idx: 0,
    key: label,
    label,
    phase,
    prompt: "p",
    model: null,
    status: status as WfAgentView["status"],
    result: null,
    sessionId: null,
    startedAt: 0,
    finishedAt: null,
    tokens: 0,
    toolCalls: 0,
    activity: [],
  };
}

function run(phases: Array<{ title: string; detail?: string }>): WorkflowRun {
  return {
    id: "r",
    sessionId: "s",
    name: "n",
    description: "d",
    script: "",
    phases,
    status: "running",
    currentPhase: null,
    result: null,
    error: null,
    args: null,
    resumeOf: null,
    createdAt: 0,
    finishedAt: null,
  };
}

Deno.test("phaseGroups: declared order first, then undeclared, then phase-less", () => {
  const groups = phaseGroups(
    run([{ title: "Find" }, { title: "Verify", detail: "check" }, { title: "Report" }]),
    [
      agent("loose", null, "done"),
      agent("v1", "Verify", "done"),
      agent("extra", "Cleanup", "done"),
      agent("f1", "Find", "done"),
    ],
  );
  assertEquals(groups.map((g) => g.title), ["Find", "Verify", "Report", "Cleanup", ""]);
  assertEquals(groups[1].detail, "check");
  // A declared phase no agent has reached yet still shows — that is how the run's
  // shape is legible before it gets there.
  assertEquals(groups[2].agents.length, 0);
  assertEquals(groups[4].agents.map((a) => a.label), ["loose"]);
});

Deno.test("phaseGroups: a run whose meta declared nothing groups every agent once", () => {
  const groups = phaseGroups(run([]), [agent("a", "Scan", "done"), agent("b", null, "running")]);
  assertEquals(groups.map((g) => g.title), ["Scan", ""]);
  assertEquals(groups.flatMap((g) => g.agents).length, 2);
});

Deno.test("visibleAgents: f folds journal replays in with done, not with running", () => {
  const list = [
    agent("a", "P", "done"),
    agent("b", "P", "cached"),
    agent("c", "P", "running"),
    agent("d", "P", "error"),
  ];
  assertEquals(visibleAgents(list, null).length, 4);
  assertEquals(visibleAgents(list, "done").map((a) => a.label), ["a", "b"]);
  assertEquals(visibleAgents(list, "running").map((a) => a.label), ["c"]);
  assertEquals(visibleAgents(list, "error").map((a) => a.label), ["d"]);
});
