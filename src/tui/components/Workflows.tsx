// The workflows tab: a miller-column browser over a run, matching Claude Code's
// /workflows. Four levels — runs → phases → that phase's agents → one agent's
// detail — where the LEFT pane always holds the level you came from and the
// right pane holds what you selected. The stacked single-column version this
// replaced silently dropped whole phase groups once the viewport filled, which
// read as "that's the whole run" on anything bigger than a handful of agents.
// App owns the data and key handling; this renders the level it is given.
import { Box } from "ink";
import { Text } from "./Text.tsx";
import { palette } from "../theme.ts";
import type { WfAgentView, WfSummary, WireWorkflowRun } from "../api.ts";
import { clip, relTime } from "../format.ts";

/** 0 runs · 1 phases · 2 a phase's agents · 3 one agent's detail. */
export type WfLevel = 0 | 1 | 2 | 3;

/** The `f` cycle: all, then each status worth isolating on a big run. */
export const WF_FILTERS = [null, "running", "queued", "done", "error"] as const;
export type WfFilter = (typeof WF_FILTERS)[number];

/** Phases-pane width. The panel draws its own border+padding around this view,
 * so every frame measurement works off `inner` (cols minus that chrome) — using
 * the raw terminal width overflowed and Ink truncated the right-hand column. */
const LEFT_W = 20;
const PANEL_CHROME = 4;

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
    case "queued":
      // Journaled but waiting on the run's concurrency semaphore — not working yet.
      return { glyph: "◦", color: palette.muted };
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

/**
 * The pane frame, drawn by hand. Ink cannot put text inside a border, and here
 * the titles ARE the border — `╭ Phases ────┬ Explore · 9 agents ────╮` — so the
 * box is emitted as plain rows with each pane's contents pre-rendered into
 * fixed-width cells. Selection is a `❯` cursor, not an inverse bar; that is what
 * makes hand-drawing viable, since no row needs a painted full-width fill.
 */
type Cell = { text: string; color?: string; dim?: boolean; bold?: boolean };
type Row = Cell[];

/** Visible width of a row — frame math is done on plain text, not on elements. */
function rowWidth(row: Row): number {
  return row.reduce((n, c) => n + c.text.length, 0);
}

/** Pad (or hard-truncate) a row to exactly `width`, so the seams stay aligned. */
function fit(row: Row, width: number): Row {
  const w = rowWidth(row);
  if (w === width) return row;
  if (w < width) return [...row, { text: " ".repeat(width - w) }];
  const out: Row = [];
  let left = width;
  for (const c of row) {
    if (left <= 0) break;
    out.push(c.text.length <= left ? c : { ...c, text: c.text.slice(0, left) });
    left -= c.text.length;
  }
  return out;
}

/** Right-align `right` against `width` with at least one space of gap. */
function columns(left: Row, right: Row, width: number): Row {
  const gap = width - rowWidth(left) - rowWidth(right);
  return gap > 0
    ? [...left, { text: " ".repeat(gap) }, ...right]
    : [...left, { text: " " }, ...right];
}

function Line({ row }: { row: Row }) {
  return (
    <Text wrap="truncate">
      {row.map((c, i) => (
        <Text key={i} color={c.color} dimColor={c.dim} bold={c.bold}>{c.text}</Text>
      ))}
    </Text>
  );
}

/** `╭ Phases ─────┬ Explore · 9 agents ─────╮` */
function frameTop(leftTitle: string, rightTitle: string, leftW: number, rightW: number): string {
  const l = `╭ ${clip(leftTitle, leftW)} `;
  const r = `┬ ${clip(rightTitle, rightW)} `;
  return l + "─".repeat(Math.max(0, leftW + 3 - l.length)) +
    r + "─".repeat(Math.max(0, rightW + 3 - r.length)) + "╮";
}

function Panes(
  { leftTitle, rightTitle, left, right, rows, cols }: {
    leftTitle: string;
    rightTitle: string;
    left: Row[];
    right: Row[];
    rows: number;
    cols: number;
  },
) {
  const leftW = LEFT_W;
  const rightW = Math.max(20, cols - PANEL_CHROME - leftW - 7);
  return (
    <Box flexDirection="column">
      <Text dimColor wrap="truncate">{frameTop(leftTitle, rightTitle, leftW, rightW)}</Text>
      {Array.from({ length: rows }, (_, i) => (
        <Line
          key={i}
          row={[
            { text: "│ ", dim: true },
            ...fit(left[i] ?? [], leftW),
            { text: " │ ", dim: true },
            ...fit(right[i] ?? [], rightW),
            { text: " │", dim: true },
          ]}
        />
      ))}
      <Text dimColor wrap="truncate">
        {"╰" + "─".repeat(leftW + 2) + "┴" + "─".repeat(rightW + 2) + "╯"}
      </Text>
    </Box>
  );
}

/** The run header both pane levels sit under. */
function RunHeader(
  { run, agents, cols, lastLog }: {
    run: WireWorkflowRun;
    agents: WfAgentView[];
    cols: number;
    lastLog?: string;
  },
) {
  const done = agents.filter((a) => a.status === "done" || a.status === "cached").length;
  const failed = agents.filter((a) => a.status === "error").length;
  const width = Math.max(20, cols - PANEL_CHROME);
  const right = `${done}/${agents.length} agents${failed ? ` (${failed} failed)` : ""} · ${
    elapsed(run.createdAt, run.finishedAt)
  }`;
  return (
    <Box flexDirection="column">
      <Text dimColor wrap="truncate">{"─".repeat(width)}</Text>
      <Text wrap="truncate">
        <Text bold color={palette.accent}>{run.name}</Text>
        {run.status !== "running" ? <Text dimColor>{"  "}{run.status}</Text> : null}
        {run.resumeOf ? <Text dimColor>{"  rerun (≡ = replayed)"}</Text> : null}
      </Text>
      <Line
        row={columns(
          [{ text: clip(run.description, Math.max(12, width - right.length - 2)), dim: true }],
          [{ text: right, dim: true }],
          width,
        )}
      />
      {run.error
        ? <Text color={palette.error} wrap="truncate">{clip(run.error, width)}</Text>
        : null}
      {lastLog && run.status === "running"
        ? <Text dimColor wrap="truncate">▸ {clip(lastLog, width - 2)}</Text>
        : null}
    </Box>
  );
}

/**
 * The Phases pane. A phase keeps its ORDINAL until it completes — the number is
 * how you read the run's shape before it gets there — and only then becomes ✓
 * (or ✗ if it settled with a failure; a red ◐ would say "still going", which is
 * the opposite of what happened).
 */
function phaseRows(
  groups: ReturnType<typeof phaseGroups>,
  selected: number,
  cursor: boolean,
  current: string | null,
): Row[] {
  return groups.map((g, i) => {
    const done = g.agents.filter((a) => a.status === "done" || a.status === "cached").length;
    const failed = g.agents.some((a) => a.status === "error");
    const busy = g.agents.some((a) => a.status === "running" || a.status === "queued");
    const complete = g.agents.length > 0 && !busy;
    const mark = complete
      ? { text: failed ? "✗" : "✓", color: failed ? palette.error : palette.accent }
      : { text: String(i + 1), dim: true };
    return [
      { text: cursor && i === selected ? "❯ " : "  ", color: palette.accent },
      mark,
      { text: " " },
      { text: clip(g.title || "agents", 13), bold: g.title === current },
      ...(g.agents.length ? [{ text: ` ${done}/${g.agents.length}`, dim: true }] : []),
    ];
  });
}

/**
 * One phase's agents: the right pane at level 2, the left pane at level 3.
 * Columns are aligned — label, then model · tokens, then the clock hard against
 * the right edge — so a fan-out reads as a table instead of ragged text.
 */
function agentRows(
  agents: WfAgentView[],
  selected: number,
  cursor: boolean,
  width: number,
  compact: boolean,
): Row[] {
  const labelW = compact
    ? Math.max(8, width - 4)
    : Math.min(34, Math.max(14, ...agents.map((a) => a.label.length + 1)));
  return agents.map((a, i) => {
    const s = wfGlyph(a.status);
    const head: Row = [
      { text: cursor && i === selected ? "❯ " : "  ", color: palette.accent },
      { text: s.glyph, color: s.color },
      { text: " " },
      ...fit([{ text: clip(a.label, labelW) }], compact ? Math.max(1, width - 4) : labelW),
    ];
    if (compact) return head;
    // A running agent shows its live clock, not the word "running" — the glyph
    // already says running, and the number is what tells you it is wedged.
    const time = a.status === "queued" ? "queued" : elapsed(a.startedAt, a.finishedAt);
    const mid = [a.model, tokenChip(a.tokens)].filter(Boolean).join(" · ");
    return columns([...head, { text: mid, dim: true }], [{ text: time, dim: true }], width);
  });
}

/**
 * Level 3's right pane, as frame rows. The prompt is COLLAPSED by default — it
 * is the one thing here you already know (you wrote the workflow); the outcome
 * is what you opened this for, and a 30-line prompt used to push it off the
 * bottom.
 */
function detailRows(
  agent: WfAgentView,
  scroll: number,
  rows: number,
  promptOpen: boolean,
  width: number,
): Row[] {
  const s = wfGlyph(agent.status);
  const promptLines = agent.prompt.split("\n");
  const body: Row[] = [];
  const head = (text: string): Row => [{ text, bold: true }];
  const line = (
    text: string,
  ): Row => [{ text: "  " + clip(text, Math.max(8, width - 2)), dim: true }];

  body.push([
    { text: s.glyph, color: s.color },
    { text: " " + agent.status },
    {
      text: "  " + [
        agent.model,
        tokenChip(agent.tokens),
        agent.toolCalls ? `${agent.toolCalls} tool call${agent.toolCalls === 1 ? "" : "s"}` : "",
        agent.status === "queued"
          ? "waiting on the run's concurrency limit"
          : elapsed(agent.startedAt, agent.finishedAt),
      ].filter(Boolean).join(" · "),
      dim: true,
    },
  ]);
  if (agent.sessionId) {
    body.push([{ text: `session ${agent.sessionId.slice(0, 8)} — o opens it`, dim: true }]);
  }
  body.push([]);
  body.push(head(
    promptOpen
      ? "Prompt · ⏎ collapse"
      : `Prompt · ${promptLines.length} line${promptLines.length === 1 ? "" : "s"} · ⏎ expand`,
  ));
  for (const l of promptOpen ? promptLines : promptLines.slice(0, 2)) body.push(line(l));
  if (!promptOpen && promptLines.length > 2) {
    body.push(line(`… ${promptLines.length - 2} more lines`));
  }
  if (agent.activity.length > 0) {
    body.push([]);
    body.push(head("Activity"));
    for (const l of agent.activity) body.push(line(l));
  }
  body.push([]);
  body.push(head(
    agent.status === "error"
      ? "Error"
      : agent.status === "cached"
      ? "Outcome (replayed from the previous run)"
      : "Outcome",
  ));
  for (const l of (agent.result ?? "(none yet)").split("\n")) {
    body.push([{ text: "  " + clip(l, Math.max(8, width - 2)) }]);
  }

  const visible = body.slice(scroll, scroll + rows);
  if (body.length > rows) {
    visible[visible.length - 1] = [{
      text: `${scroll + visible.length}/${body.length} · j/k scroll`,
      dim: true,
    }];
  }
  return visible;
}

/** How many body lines detailRows can produce — App clamps j/k against this. */
export function agentDetailLines(agent: WfAgentView, promptOpen: boolean): number {
  const prompt = agent.prompt.split("\n").length;
  const promptRows = promptOpen ? prompt : Math.min(2, prompt) + (prompt > 2 ? 1 : 0);
  const activityRows = agent.activity.length ? agent.activity.length + 2 : 0;
  const outcomeRows = (agent.result ?? "(none yet)").split("\n").length;
  return 3 + promptRows + activityRows + 2 + outcomeRows;
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
          <Line
            key={r.id}
            row={[
              { text: sel ? "❯ " : "  ", color: palette.accent },
              { text: glyph, color },
              { text: " " },
              { text: r.name, bold: true, color: sel ? palette.accent : undefined },
              { text: "  " + clip(r.description, 44), dim: true },
              {
                text: `  ${agentCounts(r.agents)} · ${elapsed(r.createdAt, r.finishedAt)} · ${
                  relTime(r.createdAt)
                } ago`,
                dim: true,
              },
            ]}
          />
        );
      })}
      {runs.length > slice.length
        ? <Text dimColor>… {runs.length - slice.length} more</Text>
        : null}
    </Box>
  );
}

/** Per-level footer — the keys that actually do something HERE. */
function footer(level: WfLevel, running: boolean): string {
  if (level === 0) return `enter open · r rerun${running ? " · x stop · p pause" : ""}`;
  if (level === 1) {
    return `↑↓ select · enter agents${running ? " · x stop workflow · p pause" : ""} · esc back`;
  }
  if (level === 2) {
    return `↑↓ select · enter open · / filter · o session${
      running ? " · x stop agent · r restart agent · p pause" : ""
    } · esc back`;
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
    cols,
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
    cols: number;
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
  // Panel chrome + this view's header/footer/margins come off the top before the
  // panes get their height; overflowing pushes the run header off the TOP of the
  // clipped panel, which is how it once went missing entirely.
  const paneRows = Math.max(4, rows - 15);
  const rightW = Math.max(20, cols - PANEL_CHROME - LEFT_W - 7);
  const running = run.status === "running" || run.status === "paused";
  const agentTitle = `${group.title || "agents"} · ${
    filter ? `showing ${shown.length} ${filter}` : `${shown.length} agents`
  }`;

  const detail = level === 3 ? shown[agentSel] : undefined;
  const leftRows = detail
    ? agentRows(shown, agentSel, true, LEFT_W, true)
    : phaseRows(groups, phaseSel, level === 1, run.currentPhase);
  const win = windowed(leftRows, detail ? agentSel : phaseSel, paneRows);
  const rightRows = detail ? detailRows(detail, scroll, paneRows, promptOpen, rightW) : windowed(
    agentRows(shown, agentSel, level === 2, rightW, false),
    agentSel,
    paneRows,
  ).slice;

  return (
    <Box marginTop={1} flexDirection="column">
      <RunHeader run={run} agents={agents} cols={cols} lastLog={lastLog} />
      <Panes
        leftTitle={detail ? clip(group.title || "agents", 14) : "Phases"}
        rightTitle={detail ? clip(detail.label, rightW - 4) : agentTitle}
        left={win.slice}
        right={rightRows}
        rows={paneRows}
        cols={cols}
      />
      <Text dimColor wrap="truncate">{footer(level, running)}</Text>
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
