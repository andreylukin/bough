// The workflows tab: a three-level drill (runs → one run's agents by phase →
// one agent's prompt/result), Claude-Code-/workflows-style. App owns the data
// and key handling; this renders the level it is given.
import { Box } from "ink";
import { Text } from "./Text.tsx";
import { palette } from "../theme.ts";
import type { WfSummary, WireWorkflowAgent, WireWorkflowRun } from "../api.ts";
import { SelRow } from "./SelRow.tsx";
import { clip, relTime } from "../format.ts";

export type WfLevel = 0 | 1 | 2;

/**
 * The run view's flat agent order: meta-declared phases first, then extra phases
 * agents reported, then phase-less agents. App passes agents through this before
 * rendering so its selection index and RunView's rows agree.
 */
export function orderAgents(
  run: WireWorkflowRun,
  agents: WireWorkflowAgent[],
): WireWorkflowAgent[] {
  const declared = run.phases.map((p) => p.title);
  const extra = [...new Set(agents.map((a) => a.phase ?? ""))]
    .filter((p) => p && !declared.includes(p));
  return [...declared, ...extra, ""].flatMap((phase) =>
    agents.filter((a) => (a.phase ?? "") === phase)
  );
}

/** Status → (glyph, color). One place so the list, run view, and chip agree. */
export function wfGlyph(status: string): { glyph: string; color: string | undefined } {
  switch (status) {
    case "running":
      return { glyph: "◐", color: palette.accent };
    case "paused":
      return { glyph: "⏸", color: palette.warn };
    case "done":
      return { glyph: "✓", color: palette.accent };
    case "cached":
      return { glyph: "≡", color: palette.accent };
    case "error":
      return { glyph: "✗", color: palette.error };
    case "stopped":
      return { glyph: "■", color: palette.warn };
    default: // orphaned
      return { glyph: "⚠", color: palette.warn };
  }
}

/** "3/7" agents with the failed count when nonzero. */
function agentCounts(a: WfSummary["agents"]): string {
  return `${a.done}/${a.total}${a.failed ? ` (${a.failed} failed)` : ""}`;
}

function elapsed(createdAt: number, finishedAt: number | null): string {
  const ms = (finishedAt ?? Date.now()) - createdAt;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m${(s % 60).toString().padStart(2, "0")}s`;
}

function RunsList({ runs, selected, rows }: { runs: WfSummary[]; selected: number; rows: number }) {
  if (runs.length === 0) {
    return (
      <Text dimColor>
        no workflow runs in this conversation — ask for one ("use a workflow: …")
      </Text>
    );
  }
  const visible = runs.slice(0, Math.max(3, rows - 8));
  return (
    <Box flexDirection="column">
      {visible.map((r, i) => {
        const sel = i === selected;
        const { glyph, color } = wfGlyph(r.status);
        return (
          <SelRow
            key={r.id}
            sel={sel}
            right={
              <Text dimColor>
                {agentCounts(r.agents)} · {elapsed(r.createdAt, r.finishedAt)} ·{" "}
                {relTime(r.createdAt)} ago
              </Text>
            }
          >
            <Text color={sel ? undefined : color}>{glyph}</Text> <Text bold>{r.name}</Text>
            <Text dimColor>
              {"  "}
              {clip(r.description, 48)}
              {r.status === "running" && r.currentPhase ? ` · ${r.currentPhase}` : ""}
              {r.resumeOf ? " · rerun" : ""}
            </Text>
          </SelRow>
        );
      })}
      {runs.length > visible.length
        ? <Text dimColor>… {runs.length - visible.length} more</Text>
        : null}
    </Box>
  );
}

/** Level 1: the run's header + its agents grouped under phase headers. */
function RunView(
  { run, agents, selected, rows, lastLog }: {
    run: WireWorkflowRun;
    agents: WireWorkflowAgent[];
    selected: number;
    rows: number;
    lastLog?: string;
  },
) {
  const { glyph, color } = wfGlyph(run.status);
  // Order: meta-declared phases first, then any extra phases agents reported,
  // then phase-less agents. Selection indexes the flat agent order (App's list).
  const declared = run.phases.map((p) => p.title);
  const extra = [...new Set(agents.map((a) => a.phase ?? ""))]
    .filter((p) => p && !declared.includes(p));
  const groups = [...declared, ...extra, ""].map((phase) => ({
    phase,
    agents: agents.filter((a) => (a.phase ?? "") === phase),
  })).filter((g) => g.agents.length > 0 || declared.includes(g.phase));
  // Flat index base per group so selection can be mapped to rows.
  let flat = 0;
  const budget = Math.max(3, rows - 12);
  let used = 0;
  return (
    <Box flexDirection="column">
      <Text wrap="truncate">
        <Text color={color}>{glyph}</Text> <Text bold>{run.name}</Text>
        <Text dimColor>
          {"  "}{run.status} · {elapsed(run.createdAt, run.finishedAt)}
          {run.resumeOf ? " · rerun (≡ = replayed from journal)" : ""}
        </Text>
      </Text>
      <Text dimColor wrap="truncate">{run.description}</Text>
      {run.error ? <Text color={palette.error} wrap="truncate">{run.error}</Text> : null}
      {lastLog && run.status === "running"
        ? <Text dimColor wrap="truncate">▸ {lastLog}</Text>
        : null}
      {groups.map((g) => {
        const base = flat;
        flat += g.agents.length;
        if (used >= budget) return null;
        used += 1 + g.agents.length;
        return (
          <Box key={g.phase || "(no phase)"} flexDirection="column" marginTop={1}>
            <Text wrap="truncate">
              <Text bold color={g.phase === run.currentPhase ? palette.accent : undefined}>
                {g.phase || "agents"}
              </Text>
              <Text dimColor>
                {"  "}
                {g.agents.filter((a) => a.status === "done" || a.status === "cached").length}/
                {g.agents.length}
                {run.phases.find((p) => p.title === g.phase)?.detail
                  ? ` · ${run.phases.find((p) => p.title === g.phase)!.detail}`
                  : ""}
              </Text>
            </Text>
            {g.agents.map((a, i) => {
              const sel = base + i === selected;
              const s = wfGlyph(a.status);
              return (
                <SelRow
                  key={a.id}
                  sel={sel}
                  right={
                    <Text dimColor>
                      {a.model ?? ""}
                      {a.finishedAt ? ` · ${elapsed(a.startedAt, a.finishedAt)}` : " · running"}
                    </Text>
                  }
                >
                  {"  "}
                  <Text color={sel ? undefined : s.color}>{s.glyph}</Text> {a.label}
                </SelRow>
              );
            })}
          </Box>
        );
      })}
      {agents.length === 0 ? <Text dimColor>no agents yet</Text> : null}
    </Box>
  );
}

/** Level 2: one agent's prompt, outcome, and backing session. j/k scrolls. */
function AgentView(
  { agent, scroll, rows }: { agent: WireWorkflowAgent; scroll: number; rows: number },
) {
  const s = wfGlyph(agent.status);
  const body: string[] = [];
  body.push("Prompt");
  for (const l of agent.prompt.split("\n")) body.push("  " + l);
  body.push("");
  body.push(agent.status === "error" ? "Error" : agent.status === "cached" ? "Outcome (replayed from the previous run)" : "Outcome");
  for (const l of (agent.result ?? "(none yet)").split("\n")) body.push("  " + l);
  const budget = Math.max(3, rows - 10);
  const visible = body.slice(scroll, scroll + budget);
  return (
    <Box flexDirection="column">
      <Text wrap="truncate">
        <Text color={s.color}>{s.glyph}</Text> <Text bold>{agent.label}</Text>
        <Text dimColor>
          {"  "}{agent.status}
          {agent.model ? ` · ${agent.model}` : ""}
          {agent.finishedAt ? ` · ${elapsed(agent.startedAt, agent.finishedAt)}` : ""}
          {agent.phase ? ` · phase: ${agent.phase}` : ""}
        </Text>
      </Text>
      {agent.sessionId
        ? <Text dimColor wrap="truncate">session {agent.sessionId} — o opens its conversation</Text>
        : null}
      <Box flexDirection="column" marginTop={1}>
        {visible.map((l, i) => (
          <Text key={i} wrap="truncate" dimColor={l === "Prompt" || l.startsWith("Outcome") || l === "Error" ? false : undefined}>
            {l === "Prompt" || l.startsWith("Outcome") || l === "Error" ? <Text bold>{l}</Text> : l}
          </Text>
        ))}
      </Box>
      {body.length > budget
        ? (
          <Text dimColor>
            {scroll + visible.length}/{body.length} · j/k scroll
          </Text>
        )
        : null}
    </Box>
  );
}

export function Workflows(
  { runs, sel, level, run, agents, agentSel, scroll, rows, lastLog }: {
    runs: WfSummary[];
    sel: number;
    level: WfLevel;
    /** The opened run's full row + journal (levels 1–2); null while loading. */
    run: WireWorkflowRun | null;
    agents: WireWorkflowAgent[];
    agentSel: number;
    scroll: number;
    rows: number;
    lastLog?: string;
  },
) {
  return (
    <Box marginTop={1} flexDirection="column">
      {level === 0 || run === null
        ? <RunsList runs={runs} selected={sel} rows={rows} />
        : level === 1
        ? (
          <RunView
            run={run}
            agents={agents}
            selected={agentSel}
            rows={rows}
            lastLog={lastLog}
          />
        )
        : agents[agentSel]
        ? <AgentView agent={agents[agentSel]} scroll={scroll} rows={rows} />
        : <Text dimColor>agent gone — esc backs out</Text>}
      <Box marginTop={1}>
        <Text dimColor wrap="truncate">
          {level === 0
            ? "enter open · x stop · p pause/resume · r rerun (replays unchanged agents)"
            : level === 1
            ? "enter agent · o open its session · x stop · p pause/resume · r rerun · esc back"
            : "j/k scroll · o open its session · esc back"}
        </Text>
      </Box>
    </Box>
  );
}
