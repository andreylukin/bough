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
 * set, so `toneColor` maps the semantic tones onto OpenTUI's named colours. One function
 * to repoint. Clipping, windowing and number formatting come from `tui/format.ts`.
 */
import { TextAttributes } from "@opentui/core";
import type { WorkflowRun } from "../../schema/parts.ts";
import type { WorkflowAgentView } from "../../workflow/control.ts";
import type { LargeRunFlag, ReplaySummary, RunCost } from "../../workflow/report.ts";
import type { WorkflowDetail, WorkflowSummary } from "../api.ts";
import { clip, fmtTokens, plural, windowAround } from "../format.ts";

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

function toneColor(tone: Tone | undefined): { fg?: string; dim?: boolean } {
  switch (tone) {
    case "accent":
      return { fg: "green" };
    case "warn":
      return { fg: "yellow" };
    case "error":
      return { fg: "red" };
    case "info":
      return { fg: "cyan" };
    case "muted":
      return { dim: true };
    default:
      return {};
  }
}

function Line({ row }: { row: Row }) {
  return (
    <text wrapMode="none">
      {row.map((c, i) => {
        const { fg, dim } = toneColor(c.tone);
        return (
          <span
            key={i}
            fg={fg}
            attributes={(c.bold ? TextAttributes.BOLD : 0) | (dim ? TextAttributes.DIM : 0)}
          >
            {c.text}
          </span>
        );
      })}
    </text>
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

/**
 * A RUN's glyph, which is not the same question as an agent's.
 *
 * `status: "done"` on a run means only that the script returned — it says nothing
 * about whether the agents it dispatched worked. A run that lost 8 of its 9 agents
 * to a schema rejection reached `done` in 1m51s and was drawn with the same green
 * `✓` as a clean one, two columns from the text "8 failed". That is the lying
 * checkmark the subagent cards were fixed for, in the one view built to explain a
 * fan-out, so a settled run with failures is amber and says so.
 */
export function runGlyph(status: string, failed: number): { glyph: string; tone: Tone } {
  if (status === "done" && failed > 0) return { glyph: "⚠", tone: "warn" };
  return wfGlyph(status);
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
 * The replay accounting. NEVER conditional — see the header. The counts always show,
 * broken out so the arithmetic is visible (`replayed + ranLive + pending === total`).
 *
 * The server's one-line form is a SECOND row only when it carries something the counts
 * do not: a source run to compare against, or the alarm case. On a first run it restated
 * them in a different format on the very next line — `0 replayed · 2 ran live · of 2`
 * above `0 replayed, 2 ran live of 2` — which reads as a rendering bug on the one panel
 * whose job is to be believed about what was and was not re-run.
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
    ...(replay.sourceId || alarm
      ? [[{ text: `  ${replay.line}`, tone: alarm ? "error" : "muted" }] as Row]
      : []),
  ];
}

/** Tokens and agent-time, per run and per phase. Visible while it runs, not after. */
export function costRows(cost: RunCost): Row[] {
  const perPhase = cost.byPhase
    .map((p) => `${p.phase || "agents"} ${tokenChip(p.tokens) || "0 tok"}`)
    .join(" · ");
  return [[
    { text: "$ cost    ", tone: "muted" },
    { text: `${tokenChip(cost.tokens) || "0 tok"} · ${plural(cost.agents, "agent")}`, tone: "text" },
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
  // `save` is offered only on a SETTLED run, and deliberately: what it stores is the
  // script, and the reason to store one is that you watched it work.
  return [
    { key: "r", label: "rerun (replays the journal)" },
    { key: "e", label: "edit script & relaunch" },
    { key: "s", label: "save to run again by name" },
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
  const settled = detail.agents.filter((a) => a.status === "done" || a.status === "cached").length;
  const failed = detail.agents.filter((a) => a.status === "error").length;
  const { glyph, tone } = runGlyph(run.status, failed);
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
        agent.toolCalls ? plural(agent.toolCalls, "tool call") : "",
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
    text: promptOpen ? "Prompt · ⏎ collapse" : `Prompt · ${plural(prompt.length, "line")} · ⏎ expand`,
    bold: true,
  }]);
  for (const l of promptOpen ? prompt : prompt.slice(0, 2)) rows.push([{ text: `  ${l}` }]);
  if (!promptOpen && prompt.length > 2) {
    rows.push([{ text: `  … ${plural(prompt.length - 2, "more line")} `.trimEnd(), tone: "muted" }]);
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
        // LOWERCASE, and checked against the keymap: `r` is `wf.rerun`. There is no
        // `R` binding in `keys.ts` and there never was, so this row advertised a dead
        // key on the one screen whose whole job is to explain the steering loop —
        // the same defect `e` had, one level down. `BOUND_STEER_KEYS` names what the
        // tab may promise; this sentence is inside that rule now.
        : `r relaunches a NEW run from this one's journal · ${plural(detail.replay.total, "call")} ` +
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

/**
 * The keys the panel actually delivers to this tab (`keys.ts`, mode `panel`).
 *
 * The legend row is the whole discoverability strategy, so it may only name keys that
 * do something. `e` was filtered out here because the keymap did not bind it and the
 * script level was therefore unreachable; `wf.script`, `wf.filter` and `wf.openAgent`
 * are bound now, scoped `tab: ["workflows"]`, so `e` joins the set and `f` and `o` are
 * named per level below. `s` (`wf.save`) joined it the same way — the saved-workflow
 * routes and `api.saveWorkflowAs` existed all along with no key and no legend, which
 * is the same dead-surface shape one level down. This set is still the one line to
 * change.
 */
const BOUND_STEER_KEYS = new Set(["p", "P", "x", "r", "e", "s"]);

/** Per-level footer — the keys that do something HERE, plus the steering controls. */
export function footer(level: WfLevel, detail: WorkflowDetail | null): string {
  const steer = detail
    ? steerActions(detail.workflow.status, detail.live)
      .filter((a) => BOUND_STEER_KEYS.has(a.key))
      .map((a) => `${a.key} ${a.label}`).join(" · ")
    : "";
  // Level 0 has no `detail` — it is not fetched until a run is opened — so the steering
  // verbs are named from the keymap instead of from a run's state. They ACT at this
  // level (`PanelHost` steers the selected row), and a verb that works and is never
  // printed is a verb nobody has.
  if (level === 0) {
    return `⏎ open · 1-9 pick · ${steer || "p pause · P resume · x stop · r relaunch"}`;
  }
  if (level === 4) return `↑↓ scroll · ${steer} · esc back`;
  if (level === 1) return `↑↓ phase · ⏎ agents · f filter · ${steer} · esc back`;
  if (level === 2) return `↑↓ agent · ⏎ open · o session · f filter · ${steer} · esc back`;
  return `⏎ prompt · ↑↓ scroll · o session · ${steer} · esc back`;
}

/**
 * Rows the level-0 run list may paint.
 *
 * The tab's own chrome is three rows: the `marginTop` above the list, the one above
 * the footer, and the footer. It used to ask for `Math.max(3, rows - 6)` — a floor
 * of three rows at ANY height, which below six rows was a request for rows that do
 * not exist, and OpenTUI answers that by shrinking rows onto each other
 * (`Panel.tsx`). Exported so `PanelHost` resolves `1`–`9` against the same window.
 */
export function wfRunsHeight(rows: number): number {
  return Math.max(0, rows - 1 - 2 * wfGap(rows));
}

/**
 * The blank separator rows, which a cramped panel cannot afford.
 *
 * Level 0 spends one above the list and one above the footer. At six body rows that
 * is a third of the tab given to whitespace, and at three it cost the footer its row
 * entirely — the tab rendered its empty-state sentence and no legend at all. Breathing
 * room is the first thing to go, and the legend is the last.
 */
export function wfGap(rows: number): number {
  return rows >= 8 ? 1 : 0;
}

function RunsList(
  { runs, sel, rows, now }: {
    runs: WorkflowSummary[];
    sel: number;
    rows: number;
    now: number;
  },
) {
  if (runs.length === 0) {
    return (
      <text attributes={TextAttributes.DIM}>
        no workflow runs in this conversation — ask for one
      </text>
    );
  }
  const height = wfRunsHeight(rows);
  const { slice, from } = height === 0
    ? { slice: [] as WorkflowSummary[], from: 0 }
    : windowed(runs, sel, height);
  return (
    <box flexDirection="column">
      {slice.map((r, i) => {
        const on = from + i === sel;
        const a = r.agents;
        const { glyph, tone } = runGlyph(r.status, a.failed);
        return (
          <Line
            key={r.id}
            row={[
              // The digit that picks this run, printed on it (spec §3).
              { text: i < 9 ? `${i + 1} ` : "  ", tone: "muted" },
              { text: on ? "❯ " : "  ", tone: "info" },
              { text: glyph, tone },
              { text: ` ${r.name}`, bold: true },
              { text: `  ${clip(r.description, 44)}`, tone: "muted" },
              {
                // The clock is half of "expensive things get a bar": a run with a
                // counter and no elapsed time cannot be told from a wedged one.
                text: `  ${a.done}/${a.total}` +
                  `${a.cached ? ` · ${a.cached} replayed` : ""}` +
                  `${a.failed ? ` · ${a.failed} failed` : ""}` +
                  ` · ${elapsed(r.createdAt, r.finishedAt, now)}`,
                tone: "muted",
              },
            ]}
          />
        );
      })}
    </box>
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
  const now = props.now ?? Date.now();
  if (level === 0 || detail === null) {
    const gap = wfGap(rows);
    return (
      <box marginTop={gap} flexDirection="column">
        <RunsList runs={props.runs} sel={props.sel} rows={rows} now={now} />
        <box marginTop={gap}>
          <text attributes={TextAttributes.DIM} wrapMode="none">{footer(0, detail)}</text>
        </box>
      </box>
    );
  }

  const allHeader = runHeaderRows(detail, {
    ...(props.lastLog ? { lastLog: props.lastLog } : {}),
    now,
  });

  const gap = wfGap(rows);
  if (level === 4) {
    // gap + header + body + footer.
    const header = allHeader.slice(0, Math.max(0, rows - gap - 2));
    const paneRows = Math.max(0, rows - gap - 1 - header.length);
    const body = scriptRows(detail);
    return (
      <box marginTop={gap} flexDirection="column">
        {header.map((r, i) => <Line key={i} row={r} />)}
        {(paneRows === 0 ? [] : windowed(body, props.scroll, paneRows).slice)
          .map((r, i) => <Line key={i} row={r} />)}
        <text attributes={TextAttributes.DIM} wrapMode="none">{footer(4, detail)}</text>
      </box>
    );
  }

  // gap + header + gap + 1 column title + panes + footer. The header is clipped rather
  // than allowed to push the panes past the bottom: a run's header can be nine rows on
  // its own, and a pane budget floored at four (`Math.max(4, …)`) was a claim about
  // space that the tab did not have.
  const header = allHeader.slice(0, Math.max(0, rows - 2 * gap - 3));
  const paneRows = Math.max(0, rows - 2 * gap - 2 - header.length);
  const leftW = Math.min(24, Math.max(12, Math.floor(cols / 4)));

  const groups = phaseGroups(detail.workflow, detail.agents);
  const group = groups[Math.min(props.phaseSel, Math.max(0, groups.length - 1))] ??
    { title: "", agents: [] };
  const shown = visibleAgents(group.agents, props.filter);
  const agent = level === 3 ? shown[props.agentSel] : undefined;

  const left = agent
    ? agentRows(shown, props.agentSel, true, true, now)
    : phaseRows(groups, props.phaseSel, level === 1, detail.workflow.currentPhase);
  const pane = (list: Row[], sel: number): Row[] =>
    paneRows === 0 ? [] : windowed(list, sel, paneRows).slice;
  const right = agent
    ? pane(agentDetailRows(agent, props.promptOpen, now), props.scroll)
    : pane(agentRows(shown, props.agentSel, level === 2, false, now), props.agentSel);

  return (
    <box marginTop={gap} flexDirection="column">
      {header.map((r, i) => <Line key={`h${i}`} row={r} />)}
      <box flexDirection="row" marginTop={gap}>
        <box flexDirection="column" width={leftW} marginRight={2}>
          <text attributes={TextAttributes.DIM} wrapMode="none">
            {agent ? clip(group.title || "agents", leftW) : "Phases"}
          </text>
          {pane(left, agent ? props.agentSel : props.phaseSel)
            .map((r, i) => <Line key={i} row={r} />)}
        </box>
        <box flexDirection="column" flexGrow={1}>
          <text attributes={TextAttributes.DIM} wrapMode="none">
            {agent
              ? clip(agent.label, 40)
              : `${group.title || "agents"} · ${shown.length}${
                props.filter ? ` ${props.filter}` : ""
              }`}
          </text>
          {right.map((r, i) => <Line key={i} row={r} />)}
        </box>
      </box>
      <text attributes={TextAttributes.DIM} wrapMode="none">{footer(level, detail)}</text>
    </box>
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
