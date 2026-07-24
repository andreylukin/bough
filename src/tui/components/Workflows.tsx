// The workflows tab: a miller-column browser over a run, matching Claude Code's
// /workflows. Four levels — runs → phases → that phase's agents → one agent's
// detail — where the LEFT pane always holds the level you came from and the
// right pane holds what you selected. The stacked single-column version this
// replaced silently dropped whole phase groups once the viewport filled, which
// read as "that's the whole run" on anything bigger than a handful of agents.
// App owns the data and key handling; this renders the level it is given.
import { Box } from "ink";
import type { ReactNode } from "react";
import { Text } from "./Text.tsx";
import { palette } from "../theme.ts";
import type { WfAgentView, WfSummary, WireWorkflowRun } from "../api.ts";
import { SelRow } from "./SelRow.tsx";
import { clip, relTime } from "../format.ts";

/** 0 runs · 1 phases · 2 a phase's agents · 3 one agent's detail. */
export type WfLevel = 0 | 1 | 2 | 3;

/** The `f` cycle: all, then each status worth isolating on a big run. */
export const WF_FILTERS = [null, "running", "done", "error"] as const;
export type WfFilter = (typeof WF_FILTERS)[number];

const LEFT_W = 26;

/**
 * The run's agents grouped under their phase, in script order: meta-declared
 * phases first (including ones no agent has reached yet, so the shape of the run
 * is visible before it gets there), then phases agents reported that meta never
 * declared, then phase-less agents.
 */
export function phaseGroups(
  run: WireWorkflowRun,
  agents: WfAgentView[],
): Array<{ title: string; detail?: string; agents: WfAgentView[] }> {
  const declared = run.phases.map((p) => p.title);
  const extra = [...new Set(agents.map((a) => a.phase ?? ""))]
    .filter((p) => p && !declared.includes(p));
  return [...declared, ...extra, ""]
    .map((title) => {
      const detail = run.phases.find((p) => p.title === title)?.detail;
      return {
        title,
        ...(detail ? { detail } : {}),
        agents: agents.filter((a) => (a.phase ?? "") === title),
      };
    })
    .filter((g) => g.agents.length > 0 || declared.includes(g.title));
}

/** The agents `f` leaves visible in a group. "done" folds in journal replays. */
export function visibleAgents(agents: WfAgentView[], filter: WfFilter): WfAgentView[] {
  if (!filter) return agents;
  if (filter === "done") {
    return agents.filter((a) => a.status === "done" || a.status === "cached");
  }
  return agents.filter((a) => a.status === filter);
}

/** Status → (glyph, color). One place so every level and the chip agree. */
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

/** "17.8k tok" — the per-agent cost signal the run view leads with. */
export function tokenChip(n: number): string {
  if (n <= 0) return "";
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k tok` : `${n} tok`;
}

/** Window a list around the cursor so a 200-agent phase still scrolls. */
function windowed<T>(items: T[], sel: number, rows: number): { slice: T[]; from: number } {
  if (items.length <= rows) return { slice: items, from: 0 };
  const from = Math.min(Math.max(0, sel - Math.floor(rows / 2)), items.length - rows);
  return { slice: items.slice(from, from + rows), from };
}

/** A pane's title line — the border can't carry text, so it sits above the rows. */
function PaneTitle({ text }: { text: string }) {
  return <Text dimColor wrap="truncate">{text}</Text>;
}

/** Left column + vertical rule + right column: the miller layout every level uses. */
function Panes(
  { left, right, rows }: { left: ReactNode; right: ReactNode; rows: number },
) {
  return (
    <Box marginTop={1} height={Math.max(4, rows)}>
      <Box
        flexDirection="column"
        width={LEFT_W}
        flexShrink={0}
        borderStyle="round"
        borderColor={palette.muted2}
        borderTop={false}
        borderBottom={false}
        borderLeft={false}
        paddingRight={1}
        overflowY="hidden"
      >
        {left}
      </Box>
      <Box flexDirection="column" flexGrow={1} paddingLeft={1} overflowY="hidden">
        {right}
      </Box>
    </Box>
  );
}

function RunsList({ runs, selected, rows }: { runs: WfSummary[]; selected: number; rows: number }) {
  if (runs.length === 0) {
    return (
      <Text dimColor>
        no workflow runs in this conversation — ask for one ("use a workflow: …")
      </Text>
    );
  }
  const { slice, from } = windowed(runs, selected, Math.max(3, rows - 6));
  return (
    <Box flexDirection="column">
      {slice.map((r, i) => {
        const sel = from + i === selected;
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
      {runs.length > slice.length
        ? <Text dimColor>… {runs.length - slice.length} more</Text>
        : null}
    </Box>
  );
}

/** The run header both pane levels sit under. */
function RunHeader({ run, lastLog }: { run: WireWorkflowRun; lastLog?: string }) {
  const { glyph, color } = wfGlyph(run.status);
  return (
    <Box flexDirection="column">
      <Text wrap="truncate">
        <Text color={color}>{glyph}</Text> <Text bold>{run.name}</Text>
        <Text dimColor>
          {"  "}
          {run.status} · {elapsed(run.createdAt, run.finishedAt)}
          {run.resumeOf ? " · rerun (≡ = replayed from journal)" : ""}
        </Text>
      </Text>
      <Text dimColor wrap="truncate">{run.description}</Text>
      {run.error ? <Text color={palette.error} wrap="truncate">{run.error}</Text> : null}
      {lastLog && run.status === "running"
        ? <Text dimColor wrap="truncate">▸ {lastLog}</Text>
        : null}
    </Box>
  );
}

/** The Phases pane: unstarted phases keep their ordinal so the plan reads ahead. */
function PhaseList(
  { groups, selected, cursor, current }: {
    groups: ReturnType<typeof phaseGroups>;
    selected: number;
    /** Draw the selection bar (level 1) vs just mark which phase is open. */
    cursor: boolean;
    current: string | null;
  },
) {
  return (
    <Box flexDirection="column">
      <PaneTitle text="Phases" />
      {groups.map((g, i) => {
        const done = g.agents.filter((a) => a.status === "done" || a.status === "cached").length;
        const sel = i === selected;
        const started = g.agents.length > 0;
        // A phase that finished with a failure reads as ✗, not a red ◐ — the
        // half-moon says "still going", which is the opposite of what happened.
        const failed = g.agents.some((a) => a.status === "error");
        const settled = !g.agents.some((a) => a.status === "running");
        const state = failed && settled ? "error" : done === g.agents.length ? "done" : "running";
        const glyph = wfGlyph(state);
        return (
          <SelRow key={g.title || "(none)"} sel={sel && cursor}>
            {started
              ? (
                <Text color={sel && cursor ? undefined : glyph.color}>
                  {glyph.glyph}
                </Text>
              )
              : <Text dimColor>{i + 1}</Text>}{" "}
            <Text bold={g.title === current}>{clip(g.title || "agents", 13)}</Text>
            {started ? <Text dimColor>{`  ${done}/${g.agents.length}`}</Text> : null}
          </SelRow>
        );
      })}
    </Box>
  );
}

/** One phase's agents: the right pane at level 2, the left pane at level 3. */
function AgentList(
  { group, agents, selected, cursor, rows, filter, compact }: {
    group: { title: string; detail?: string };
    agents: WfAgentView[];
    selected: number;
    cursor: boolean;
    rows: number;
    filter: WfFilter;
    /** Left-pane mode: labels only, no model/token/time column. */
    compact?: boolean;
  },
) {
  const title = `${group.title || "agents"} · ${
    filter ? `showing ${agents.length} ${filter}` : `${agents.length} agents`
  }`;
  const { slice, from } = windowed(agents, selected, Math.max(2, rows - 2));
  return (
    <Box flexDirection="column">
      <PaneTitle text={compact ? clip(title, LEFT_W - 2) : title} />
      {agents.length === 0
        ? <Text dimColor>{filter ? "none with that status — f cycles" : "no agents yet"}</Text>
        : null}
      {slice.map((a, i) => {
        const sel = from + i === selected;
        const s = wfGlyph(a.status);
        return (
          <SelRow
            key={a.id}
            sel={sel && cursor}
            right={compact ? undefined : (
              <Text dimColor>
                {[
                  a.model,
                  tokenChip(a.tokens),
                  a.finishedAt ? elapsed(a.startedAt, a.finishedAt) : "running",
                ].filter(Boolean).join(" · ")}
              </Text>
            )}
          >
            <Text color={sel && cursor ? undefined : s.color}>{s.glyph}</Text>{" "}
            {clip(a.label, compact ? LEFT_W - 6 : 48)}
          </SelRow>
        );
      })}
      {agents.length > slice.length
        ? <Text dimColor>… {agents.length - slice.length} more · ↑↓ scrolls</Text>
        : null}
    </Box>
  );
}

/**
 * How many body lines an agent's detail has, so App can clamp j/k at the last
 * screenful instead of scrolling off into a blank pane. Kept next to the render
 * that builds those lines — the two must not drift.
 */
export function agentDetailLines(agent: WfAgentView, promptOpen: boolean): number {
  const prompt = agent.prompt.split("\n").length;
  const promptRows = promptOpen ? prompt : Math.min(2, prompt) + (prompt > 2 ? 1 : 0);
  const activityRows = agent.activity.length ? agent.activity.length + 2 : 0;
  const outcomeRows = (agent.result ?? "(none yet)").split("\n").length;
  return 1 + promptRows + activityRows + 2 + outcomeRows;
}

/**
 * Level 3's right pane. The prompt is COLLAPSED by default — it is the one thing
 * here you already know (you wrote the workflow); the outcome is what you opened
 * this for, and a 30-line prompt used to push it off the bottom.
 */
function AgentDetail(
  { agent, scroll, rows, promptOpen }: {
    agent: WfAgentView;
    scroll: number;
    rows: number;
    promptOpen: boolean;
  },
) {
  const s = wfGlyph(agent.status);
  const promptLines = agent.prompt.split("\n");
  const body: Array<{ text: string; head?: boolean }> = [];
  body.push({
    text: promptOpen
      ? "Prompt · ⏎ collapse"
      : `Prompt · ${promptLines.length} line${promptLines.length === 1 ? "" : "s"} · ⏎ expand`,
    head: true,
  });
  for (const l of promptOpen ? promptLines : promptLines.slice(0, 2)) body.push({ text: "  " + l });
  if (!promptOpen && promptLines.length > 2) {
    body.push({ text: `  … ${promptLines.length - 2} more lines` });
  }
  if (agent.activity.length > 0) {
    body.push({ text: " " });
    body.push({ text: "Activity", head: true });
    for (const l of agent.activity) body.push({ text: "  " + l });
  }
  body.push({ text: " " });
  body.push({
    text: agent.status === "error"
      ? "Error"
      : agent.status === "cached"
      ? "Outcome (replayed from the previous run)"
      : "Outcome",
    head: true,
  });
  for (const l of (agent.result ?? "(none yet)").split("\n")) body.push({ text: "  " + l });

  const budget = Math.max(3, rows - 4);
  const visible = body.slice(scroll, scroll + budget);
  return (
    <Box flexDirection="column">
      <PaneTitle text={clip(agent.label, 60)} />
      <Text wrap="truncate">
        <Text color={s.color}>{s.glyph}</Text> <Text>{agent.status}</Text>
        <Text dimColor>
          {"  "}
          {[
            agent.model,
            tokenChip(agent.tokens),
            agent.toolCalls
              ? `${agent.toolCalls} tool call${agent.toolCalls === 1 ? "" : "s"}`
              : "",
            agent.finishedAt ? elapsed(agent.startedAt, agent.finishedAt) : "running",
          ].filter(Boolean).join(" · ")}
        </Text>
      </Text>
      {agent.sessionId
        ? <Text dimColor wrap="truncate">session {agent.sessionId.slice(0, 8)} — o opens it</Text>
        : null}
      <Box flexDirection="column" marginTop={1}>
        {visible.map((l, i) => <Text key={i} wrap="truncate" bold={l.head}>{l.text}</Text>)}
      </Box>
      {body.length > budget
        ? <Text dimColor>{scroll + visible.length}/{body.length} · j/k scroll</Text>
        : null}
    </Box>
  );
}

/** Per-level footer — the keys that actually do something HERE. */
function footer(level: WfLevel, running: boolean): string {
  if (level === 0) return `enter open · r rerun${running ? " · x stop · p pause" : ""}`;
  if (level === 1) {
    return `↑↓ phase · enter agents · esc back${running ? " · x stop · p pause" : ""}`;
  }
  if (level === 2) {
    return `↑↓ agent · enter open · f filter · o session · esc back${
      running ? " · x stop agent · r restart agent · p pause" : ""
    }`;
  }
  return "↑↓ agent · ⏎ prompt · j/k scroll · o session · esc back";
}

export function Workflows(
  {
    runs,
    sel,
    level,
    run,
    agents,
    phaseSel,
    agentSel,
    scroll,
    filter,
    promptOpen,
    rows,
    lastLog,
  }: {
    runs: WfSummary[];
    sel: number;
    level: WfLevel;
    /** The opened run's full row + journal (levels 1–3); null while loading. */
    run: WireWorkflowRun | null;
    agents: WfAgentView[];
    phaseSel: number;
    agentSel: number;
    scroll: number;
    filter: WfFilter;
    promptOpen: boolean;
    rows: number;
    lastLog?: string;
  },
) {
  if (level === 0 || run === null) {
    return (
      <Box marginTop={1} flexDirection="column">
        <RunsList runs={runs} selected={sel} rows={rows} />
        <Box marginTop={1}>
          <Text dimColor wrap="truncate">{footer(0, runs[sel]?.status === "running")}</Text>
        </Box>
      </Box>
    );
  }
  const groups = phaseGroups(run, agents);
  const group = groups[Math.min(phaseSel, Math.max(0, groups.length - 1))] ??
    { title: "", agents: [] };
  const shown = visibleAgents(group.agents, filter);
  // Panel chrome (border, tab bar) + this view's own header/footer/margins come
  // off the top before the panes get their height; overflowing pushes the run
  // header off the TOP of the clipped panel, which is how it went missing.
  const paneRows = Math.max(4, rows - 13);
  const running = run.status === "running" || run.status === "paused";
  return (
    <Box marginTop={1} flexDirection="column">
      <RunHeader run={run} lastLog={lastLog} />
      {level === 3 && shown[agentSel]
        ? (
          <Panes
            rows={paneRows}
            left={
              <AgentList
                group={group}
                agents={shown}
                selected={agentSel}
                cursor
                rows={paneRows}
                filter={filter}
                compact
              />
            }
            right={
              <AgentDetail
                agent={shown[agentSel]}
                scroll={scroll}
                rows={paneRows}
                promptOpen={promptOpen}
              />
            }
          />
        )
        : (
          <Panes
            rows={paneRows}
            left={
              <PhaseList
                groups={groups}
                selected={phaseSel}
                cursor={level === 1}
                current={run.currentPhase}
              />
            }
            right={
              <AgentList
                group={group}
                agents={shown}
                selected={agentSel}
                cursor={level === 2}
                rows={paneRows}
                filter={filter}
              />
            }
          />
        )}
      <Box marginTop={1}>
        <Text dimColor wrap="truncate">{footer(level, running)}</Text>
      </Box>
    </Box>
  );
}

/**
 * The composer's live-run line: what is running, without opening the panel.
 * Carries the elapsed clock so a wedged run is visible from the chat view —
 * "3/8 agents" alone looks identical at 10 seconds and at 10 minutes.
 */
export function WorkflowChip({ run, log }: { run: WfSummary; log?: string }) {
  const { glyph, color } = wfGlyph(run.status);
  return (
    <Text wrap="truncate">
      <Text color={color}>{glyph}</Text> <Text bold>{run.name}</Text>
      <Text dimColor>
        {"  "}
        {agentCounts(run.agents)} agents · {elapsed(run.createdAt, run.finishedAt)}
        {run.currentPhase ? ` · ${run.currentPhase}` : ""}
        {log ? ` · ${clip(log, 40)}` : ""}
        {" · /workflows"}
      </Text>
    </Text>
  );
}
