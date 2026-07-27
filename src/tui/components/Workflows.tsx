/**
 * The workflow run view — the surface delegation is *for*.
 *
 * THE INVARIANT THIS HOLDS: **a run that replayed nothing never looks like a run that
 * worked.** Spec §8 states it as a requirement on the system, not on the UI: "Any
 * operation that replays returns how many calls were served from the journal and how
 * many ran live… A rerun that silently replayed nothing looks exactly like a successful
 * rerun, so the count is the only thing that makes a key defect visible. This is a
 * required part of the response, not a UI nicety." The server already computes it
 * (`workflow/report.ts`); the failure mode this file prevents is the client dropping it
 * on the floor. `replayRows` is therefore unconditional — it renders for every run, in
 * every state, before the phases — and it renders the counts AND the server's canonical
 * `line` so a bough TUI, `bough exec` and a system note all say the same sentence.
 *
 * The alarm case gets its own tone: `available > 0, replayed: 0` means the source run
 * held answers and this run's keys matched none of them, which is a key defect and not
 * a slow day. `available: 0` is an ordinary first run and says so quietly.
 *
 * WHAT THE VIEW IS. Miller-column: runs → phases → that phase's agents → one agent,
 * plus the script. The stacked single-column version this replaces silently dropped
 * whole phase groups once the viewport filled, which read as "that is the whole run".
 * Phases keep their ordinal until they complete, so the SHAPE of a run is legible
 * before it gets there — `meta.phases` declares stages no agent has reached yet.
 *
 * STEERING (spec §8). `p` pauses — gating new `agent()` calls while the ones in flight
 * finish and are journaled — and `x` stops. The script level exists because "stop, edit
 * the script, relaunch seeded from the journal" is the whole steering loop, and it names
 * the edit target on screen: the mirror at `~/.bough/workflows/<id>.js`. Pausing BEFORE
 * stopping is called out, since it preserves the most work — a dispatched agent allowed
 * to finish is journaled and replays; one killed in flight is not.
 *
 * COST IS A HEADER FIELD, not a post-mortem: tokens per agent and per phase while the
 * run is going, and the advisory large-run flag beside the control that stops it — a
 * warning with no adjacent action is one people learn to ignore.
 *
 * PURE ROWS. Everything above the component is `Row[]` — `{text, tone}` cells with no
 * React in them — so what the user sees is asserted directly in `Workflows.test.ts`
 * with nothing mounted and no terminal (plan §7).
 *
 * NOTE on colour: `tui/theme.ts` (T9.2) has not landed and is not in this task's owned
 * set, so `toneColor` maps the semantic tones onto ink's named colours. One function to
 * repoint. Clipping, windowing and number formatting come from `tui/format.ts`.
 */
import { Box, Text } from "ink";
import type { WorkflowRun } from "../../schema/parts.ts";
import type { WorkflowAgentView } from "../../workflow/control.ts";
import type { LargeRunFlag, ReplaySummary, RunCost } from "../../workflow/report.ts";
import type { WorkflowDetail, WorkflowSummary } from "../api.ts";
import { clip, fmtTokens, windowAround } from "../format.ts";

// ---------------------------------------------------------------------------
// Rows: the testable rendering unit
// ---------------------------------------------------------------------------

export type Tone = "text" | "muted" | "accent" | "warn" | "error" | "info";
export interface Cell {
  text: string;
  tone?: Tone;
  bold?: boolean;
}
export type Row = Cell[];

export const rowText = (row: Row): string => row.map((c) => c.text).join("");
export const linesOf = (rows: Row[]): string[] => rows.map(rowText);

function toneColor(tone: Tone | undefined): { color?: string; dimColor?: boolean } {
  switch (tone) {
    case "accent":
      return { color: "green" };
    case "warn":
      return { color: "yellow" };
    case "error":
      return { color: "red" };
    case "info":
      return { color: "cyan" };
    case "muted":
      return { dimColor: true };
    default:
      return {};
  }
}

function Line({ row }: { row: Row }) {
  return (
    <Text wrap="truncate">
      {row.map((c, i) => <Text key={i} bold={c.bold} {...toneColor(c.tone)}>{c.text}</Text>)}
    </Text>
  );
}

/** `12s` / `3m07s`. Seconds survive past a minute: the clock is how a wedged agent shows. */
function elapsed(from: number, to: number | null, now: number): string {
  const s = Math.max(0, Math.round(((to ?? now) - from) / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m${String(s % 60).padStart(2, "0")}s`;
}

/** "17.8k tok" — the per-agent cost signal. Empty for a call that spent nothing. */
export function tokenChip(n: number): string {
  return n <= 0 ? "" : `${fmtTokens(n)} tok`;
}

/** Window a list around the cursor so a 200-agent phase still scrolls. */
export function windowed<T>(items: T[], sel: number, rows: number): { slice: T[]; from: number } {
  if (items.length <= rows) return { slice: items, from: 0 };
  const { start, end } = windowAround(sel, items.length, rows);
  return { slice: items.slice(start, end), from: start };
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/** Status → (glyph, tone). One place, so every level and the chip agree. */
export function wfGlyph(status: string): { glyph: string; tone: Tone } {
  switch (status) {
    case "queued":
      return { glyph: "◦", tone: "muted" }; // journaled, waiting on the semaphore
    case "running":
      return { glyph: "◐", tone: "info" };
    case "paused":
      return { glyph: "⏸", tone: "warn" };
    case "done":
      return { glyph: "✓", tone: "accent" };
    case "cached":
      return { glyph: "≡", tone: "accent" }; // replayed from the journal — no agent ran
    case "error":
      return { glyph: "✗", tone: "error" };
    case "stopped":
      return { glyph: "■", tone: "warn" };
    default: // orphaned
      return { glyph: "⚠", tone: "warn" };
  }
}

// ---------------------------------------------------------------------------
// Grouping and filtering (pure)
// ---------------------------------------------------------------------------

export interface PhaseGroup {
  title: string;
  detail?: string;
  agents: WorkflowAgentView[];
}

/**
 * Agents grouped under their phase, in script order: `meta`-declared phases first —
 * INCLUDING ones no agent has reached, so the run's shape is visible before it gets
 * there — then phases agents reported that meta never declared, then phase-less agents.
 */
export function phaseGroups(run: WorkflowRun, agents: WorkflowAgentView[]): PhaseGroup[] {
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

/** The `f` cycle: all, then each status worth isolating on a big run. */
export const WF_FILTERS = [null, "running", "queued", "done", "error"] as const;
export type WfFilter = (typeof WF_FILTERS)[number];

/** "done" folds in journal replays: both are answers, and only one cost anything. */
export function visibleAgents(agents: WorkflowAgentView[], filter: WfFilter): WorkflowAgentView[] {
  if (!filter) return agents;
  if (filter === "done") {
    return agents.filter((a) => a.status === "done" || a.status === "cached");
  }
  return agents.filter((a) => a.status === filter);
}

// ---------------------------------------------------------------------------
// Replay, cost, and the large-run flag
// ---------------------------------------------------------------------------

/**
 * The replay accounting. NEVER conditional — see the header. Two rows: the counts,
 * broken out so the arithmetic is visible (`replayed + ranLive + pending === total`),
 * then the server's one-line form, so every client says the same sentence.
 */
export function replayRows(replay: ReplaySummary): Row[] {
  const alarm = replay.available > 0 && replay.replayed === 0;
  const counts = [
    `${replay.replayed} replayed`,
    `${replay.ranLive} ran live`,
    ...(replay.pending > 0 ? [`${replay.pending} still going`] : []),
    `of ${replay.total}`,
  ].join(" · ");
  const source = replay.sourceId ? ` · ${replay.available} available to replay` : "";
  return [
    [
      { text: "≡ replay  ", tone: "muted" },
      { text: counts + source, tone: alarm ? "error" : "text", bold: alarm },
    ],
    [{ text: `  ${replay.line}`, tone: alarm ? "error" : "muted" }],
  ];
}

/** Tokens and agent-time, per run and per phase. Visible while it runs, not after. */
export function costRows(cost: RunCost): Row[] {
  const perPhase = cost.byPhase
    .map((p) => `${p.phase || "agents"} ${tokenChip(p.tokens) || "0 tok"}`)
    .join(" · ");
  return [[
    { text: "$ cost    ", tone: "muted" },
    { text: `${tokenChip(cost.tokens) || "0 tok"} · ${cost.agents} agents`, tone: "text" },
    { text: perPhase ? `  ${perPhase}` : "", tone: "muted" },
  ]];
}

/** The advisory flag, next to the control that actually stops the run. */
export function warningRows(warning: LargeRunFlag | null): Row[] {
  if (!warning) return [];
  return [
    [
      { text: "! large   ", tone: "warn" },
      { text: warning.reasons.join(" · "), tone: "warn" },
    ],
    [{ text: "  advisory — nothing is throttled; x stops the run", tone: "muted" }],
  ];
}

// ---------------------------------------------------------------------------
// Steering (spec §8)
// ---------------------------------------------------------------------------

export interface SteerAction {
  key: string;
  label: string;
}

/**
 * The controls that apply to a run in this state. `pause` is named as the thing to do
 * BEFORE stopping: a dispatched agent allowed to finish is journaled and replays, while
 * one killed in flight is not and starts over. A terminal run offers the other half of
 * the loop — edit the script, relaunch seeded from this run's journal.
 */
export function steerActions(status: WorkflowRun["status"], live: boolean): SteerAction[] {
  const stop: SteerAction = { key: "x", label: "stop" };
  const script: SteerAction = { key: "e", label: "script" };
  if (status === "running") {
    // A run this process no longer holds cannot honor a pause, so it is not offered one.
    return live
      ? [{ key: "p", label: "pause (finishes in-flight agents)" }, stop]
      : [{ key: "x", label: "stop — orphaned by a restart" }, script];
  }
  if (status === "paused") return [{ key: "p", label: "resume" }, stop, script];
  return [
    { key: "r", label: "rerun (replays the journal)" },
    { key: "e", label: "edit script & relaunch" },
  ];
}

// ---------------------------------------------------------------------------
// Header, panes and detail (pure rows)
// ---------------------------------------------------------------------------

export function runHeaderRows(
  detail: WorkflowDetail,
  opts: { lastLog?: string; now?: number } = {},
): Row[] {
  const now = opts.now ?? Date.now();
  const run = detail.workflow;
  const { glyph, tone } = wfGlyph(run.status);
  const settled = detail.agents.filter((a) => a.status === "done" || a.status === "cached").length;
  const failed = detail.agents.filter((a) => a.status === "error").length;
  const rows: Row[] = [
    [
      { text: glyph, tone },
      { text: ` ${run.name}`, bold: true },
      { text: `  ${run.status}`, tone: run.status === "error" ? "error" : "muted" },
      { text: run.resumeOf ? `  relaunch of ${run.resumeOf}` : "", tone: "muted" },
      { text: detail.live || run.status !== "running" ? "" : "  (not live here)", tone: "warn" },
    ],
    [
      { text: run.description, tone: "muted" },
      {
        text: `  ${settled}/${detail.agents.length} agents${failed ? ` · ${failed} failed` : ""}` +
          ` · ${elapsed(run.createdAt, run.finishedAt, now)}`,
        tone: "muted",
      },
    ],
    ...replayRows(detail.replay),
    ...costRows(detail.cost),
    ...warningRows(detail.warning),
  ];
  if (run.error) rows.push([{ text: run.error, tone: "error" }]);
  if (opts.lastLog && run.status === "running") {
    rows.push([{ text: `▸ ${opts.lastLog}`, tone: "muted" }]);
  }
  return rows;
}

/**
 * The Phases pane. A phase keeps its ORDINAL until it completes — the number is how the
 * run's shape reads before it gets there — and only then becomes ✓, or ✗ when it settled
 * with a failure (a red ◐ would say "still going", the opposite of what happened).
 */
export function phaseRows(
  groups: PhaseGroup[],
  selected: number,
  cursor: boolean,
  current: string | null,
): Row[] {
  return groups.map((g, i) => {
    const done = g.agents.filter((a) => a.status === "done" || a.status === "cached").length;
    const busy = g.agents.some((a) => a.status === "running" || a.status === "queued");
    const failed = g.agents.some((a) => a.status === "error");
    const complete = g.agents.length > 0 && !busy;
    const mark: Cell = complete
      ? { text: failed ? "✗" : "✓", tone: failed ? "error" : "accent" }
      : { text: String(i + 1), tone: "muted" };
    return [
      { text: cursor && i === selected ? "❯ " : "  ", tone: "info" },
      mark,
      { text: ` ${clip(g.title || "agents", 14)}`, bold: g.title === current },
      ...(g.agents.length ? [{ text: ` ${done}/${g.agents.length}`, tone: "muted" as Tone }] : []),
    ];
  });
}

/** One phase's agents. A running agent shows its clock, not the word "running". */
export function agentRows(
  agents: WorkflowAgentView[],
  selected: number,
  cursor: boolean,
  compact: boolean,
  now: number = Date.now(),
): Row[] {
  return agents.map((a, i) => {
    const { glyph, tone } = wfGlyph(a.status);
    const head: Row = [
      { text: cursor && i === selected ? "❯ " : "  ", tone: "info" },
      { text: glyph, tone },
      { text: ` ${clip(a.label || "(unlabeled)", compact ? 16 : 34)}` },
    ];
    if (compact) return head;
    const time = a.status === "queued" ? "queued" : elapsed(a.startedAt, a.finishedAt, now);
    const mid = [a.model, tokenChip(a.tokens)].filter(Boolean).join(" · ");
    return [...head, { text: `  ${mid}`, tone: "muted" }, { text: `  ${time}`, tone: "muted" }];
  });
}

/**
 * One agent, in full. The prompt is COLLAPSED by default — it is the one thing you
 * already know, you wrote the workflow — and the outcome is what the drill-in was for.
 */
export function agentDetailRows(
  agent: WorkflowAgentView,
  promptOpen: boolean,
  now: number = Date.now(),
): Row[] {
  const { glyph, tone } = wfGlyph(agent.status);
  const prompt = agent.prompt.split("\n");
  const rows: Row[] = [[
    { text: glyph, tone },
    { text: ` ${agent.status}` },
    {
      text: "  " + [
        agent.model,
        tokenChip(agent.tokens),
        agent.toolCalls ? `${agent.toolCalls} tool calls` : "",
        agent.status === "queued"
          ? "waiting on the run's concurrency limit"
          : elapsed(agent.startedAt, agent.finishedAt, now),
      ].filter(Boolean).join(" · "),
      tone: "muted",
    },
  ]];
  rows.push([{
    text: agent.sessionId
      ? `session ${agent.sessionId.slice(0, 8)} — o opens it`
      : "no session — this call was replayed from the journal",
    tone: "muted",
  }]);
  rows.push([]);
  rows.push([{
    text: promptOpen ? "Prompt · ⏎ collapse" : `Prompt · ${prompt.length} lines · ⏎ expand`,
    bold: true,
  }]);
  for (const l of promptOpen ? prompt : prompt.slice(0, 2)) rows.push([{ text: `  ${l}` }]);
  if (!promptOpen && prompt.length > 2) {
    rows.push([{ text: `  … ${prompt.length - 2} more lines`, tone: "muted" }]);
  }
  if (agent.activity.length > 0) {
    rows.push([], [{ text: "Activity", bold: true }]);
    for (const l of agent.activity) rows.push([{ text: `  ${l}`, tone: "muted" }]);
  }
  rows.push([]);
  rows.push([{
    text: agent.status === "error"
      ? "Error"
      : agent.status === "cached"
      ? "Outcome · replayed from the source run's journal — no agent ran"
      : "Outcome",
    bold: true,
    tone: agent.status === "error" ? "error" : undefined,
  }]);
  for (const l of (agent.error ?? agent.result ?? "(none yet)").split("\n")) {
    rows.push([{ text: `  ${l}`, tone: agent.status === "error" ? "error" : undefined }]);
  }
  return rows;
}

/**
 * The script, and where to edit it. The mirror path is rendered first and in full,
 * because spec §8's loop is "stop, edit the script — on disk at
 * `~/.bough/workflows/<id>.js`, through the API, or by asking the agent to rewrite it —
 * and relaunch seeded from the stopped run's journal": the path IS the affordance. The
 * relaunch is a NEW run reading this one's journal, so the unchanged prefix replays and
 * everything from the first changed call onward runs live.
 */
export function scriptRows(detail: WorkflowDetail): Row[] {
  const lines = detail.workflow.script.split("\n");
  const width = String(lines.length).length;
  return [
    [{ text: detail.scriptFile, tone: "info" }],
    [{
      text: detail.live
        ? "the run is still live — pause, then stop, before you edit: dispatched agents that " +
          "finish are journaled and replay"
        : `R relaunches a NEW run from this one's journal · ${detail.replay.total} calls ` +
          `journaled here`,
      tone: detail.live ? "warn" : "muted",
    }],
    [],
    ...lines.map((l, i): Row => [
      { text: `${String(i + 1).padStart(width)} `, tone: "muted" },
      { text: l },
    ]),
  ];
}

// ---------------------------------------------------------------------------
// The component
// ---------------------------------------------------------------------------

/** 0 runs · 1 phases · 2 a phase's agents · 3 one agent · 4 the script. */
export type WfLevel = 0 | 1 | 2 | 3 | 4;

/** Per-level footer — the keys that do something HERE, plus the steering controls. */
export function footer(level: WfLevel, detail: WorkflowDetail | null): string {
  const steer = detail
    ? steerActions(detail.workflow.status, detail.live).map((a) => `${a.key} ${a.label}`).join(
      " · ",
    )
    : "";
  if (level === 0) return `⏎ open · ${steer || "r rerun"}`;
  if (level === 4) return `${steer} · esc back`;
  if (level === 1) return `↑↓ phase · ⏎ agents · ${steer} · esc back`;
  if (level === 2) return `↑↓ agent · ⏎ open · f filter · o session · ${steer} · esc back`;
  return `⏎ prompt · j/k scroll · o session · ${steer} · esc back`;
}

function RunsList({ runs, sel, rows }: { runs: WorkflowSummary[]; sel: number; rows: number }) {
  if (runs.length === 0) {
    return <Text dimColor>no workflow runs in this conversation — ask for one</Text>;
  }
  const { slice, from } = windowed(runs, sel, Math.max(3, rows - 6));
  return (
    <Box flexDirection="column">
      {slice.map((r, i) => {
        const on = from + i === sel;
        const { glyph, tone } = wfGlyph(r.status);
        const a = r.agents;
        return (
          <Line
            key={r.id}
            row={[
              { text: on ? "❯ " : "  ", tone: "info" },
              { text: glyph, tone },
              { text: ` ${r.name}`, bold: true },
              { text: `  ${clip(r.description, 44)}`, tone: "muted" },
              {
                text: `  ${a.done}/${a.total}` +
                  `${a.cached ? ` · ${a.cached} replayed` : ""}` +
                  `${a.failed ? ` · ${a.failed} failed` : ""}`,
                tone: "muted",
              },
            ]}
          />
        );
      })}
    </Box>
  );
}

export interface WorkflowsProps {
  runs: WorkflowSummary[];
  sel: number;
  level: WfLevel;
  /** `GET /workflows/:id` for the opened run; null at level 0 or while loading. */
  detail: WorkflowDetail | null;
  phaseSel: number;
  agentSel: number;
  scroll: number;
  filter: WfFilter;
  promptOpen: boolean;
  rows: number;
  cols: number;
  lastLog?: string;
  /** Injected so a render is reproducible in a test. */
  now?: number;
}

export function Workflows(props: WorkflowsProps) {
  const { detail, level, rows, cols } = props;
  if (level === 0 || detail === null) {
    return (
      <Box marginTop={1} flexDirection="column">
        <RunsList runs={props.runs} sel={props.sel} rows={rows} />
        <Box marginTop={1}>
          <Text dimColor wrap="truncate">{footer(0, detail)}</Text>
        </Box>
      </Box>
    );
  }

  const now = props.now ?? Date.now();
  const header = runHeaderRows(detail, {
    ...(props.lastLog ? { lastLog: props.lastLog } : {}),
    now,
  });
  const paneRows = Math.max(4, rows - header.length - 4);
  const leftW = Math.min(24, Math.max(12, Math.floor(cols / 4)));

  if (level === 4) {
    const body = scriptRows(detail);
    return (
      <Box marginTop={1} flexDirection="column">
        {header.map((r, i) => <Line key={i} row={r} />)}
        {windowed(body, props.scroll, paneRows).slice.map((r, i) => <Line key={i} row={r} />)}
        <Text dimColor wrap="truncate">{footer(4, detail)}</Text>
      </Box>
    );
  }

  const groups = phaseGroups(detail.workflow, detail.agents);
  const group = groups[Math.min(props.phaseSel, Math.max(0, groups.length - 1))] ??
    { title: "", agents: [] };
  const shown = visibleAgents(group.agents, props.filter);
  const agent = level === 3 ? shown[props.agentSel] : undefined;

  const left = agent
    ? agentRows(shown, props.agentSel, true, true, now)
    : phaseRows(groups, props.phaseSel, level === 1, detail.workflow.currentPhase);
  const right = agent
    ? windowed(agentDetailRows(agent, props.promptOpen, now), props.scroll, paneRows).slice
    : windowed(agentRows(shown, props.agentSel, level === 2, false, now), props.agentSel, paneRows)
      .slice;

  return (
    <Box marginTop={1} flexDirection="column">
      {header.map((r, i) => <Line key={`h${i}`} row={r} />)}
      <Box flexDirection="row" marginTop={1}>
        <Box flexDirection="column" width={leftW} marginRight={2}>
          <Text dimColor wrap="truncate">
            {agent ? clip(group.title || "agents", leftW) : "Phases"}
          </Text>
          {windowed(left, agent ? props.agentSel : props.phaseSel, paneRows).slice
            .map((r, i) => <Line key={i} row={r} />)}
        </Box>
        <Box flexDirection="column" flexGrow={1}>
          <Text dimColor wrap="truncate">
            {agent
              ? clip(agent.label, 40)
              : `${group.title || "agents"} · ${shown.length}${
                props.filter ? ` ${props.filter}` : ""
              }`}
          </Text>
          {right.map((r, i) => <Line key={i} row={r} />)}
        </Box>
      </Box>
      <Text dimColor wrap="truncate">{footer(level, detail)}</Text>
    </Box>
  );
}

/**
 * The composer's live-run line: what is running, without opening the panel. Carries the
 * replayed count too — a chip that says "3/8 agents" hides whether any of them cost
 * anything.
 */
export function WorkflowChip(
  { run, log, now }: { run: WorkflowSummary; log?: string; now?: number },
) {
  const { glyph, tone } = wfGlyph(run.status);
  const a = run.agents;
  return (
    <Line
      row={[
        { text: glyph, tone },
        { text: ` ${run.name}`, bold: true },
        {
          text: `  ${a.done}/${a.total} agents${a.cached ? ` · ${a.cached} replayed` : ""}` +
            ` · ${elapsed(run.createdAt, run.finishedAt, now ?? Date.now())}` +
            `${run.currentPhase ? ` · ${run.currentPhase}` : ""}` +
            `${log ? ` · ${clip(log, 40)}` : ""} · /workflows`,
          tone: "muted",
        },
      ]}
    />
  );
}
