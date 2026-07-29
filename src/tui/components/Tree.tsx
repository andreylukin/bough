/**
 * The one tree, painted.
 *
 * THE INVARIANT THIS HOLDS: **visibility is derived from lineage, never stored.**
 * Spec §4 is explicit — sessions of kind `subagent` and `workflow_agent` collapse
 * under their origin and surface only on drill-in; roots and their branches are
 * always listed; and "there is no archive, deprecate, hide, or purge action, and no
 * corresponding columns". The old tree rendered a `deprecatedAt` strikethrough, a
 * `showDeprecated` toggle and an `x deprecate` binding. None of that is ported: the
 * state does not exist, so neither does the affordance.
 *
 * PURE CORE ELSEWHERE. The rows come from `forest.ts`, which is a fold over
 * sessions and threads with no React, no clock and no I/O (plan §7: "renderers over
 * fixture state"). This file is the thin part: it windows the list around the
 * cursor and paints it. Measurement and clipping come from `format.ts` — display
 * width is never `String.length`.
 */
import { TextAttributes } from "@opentui/core";
import type { SessionKind } from "../../schema/parts.ts";
import type { SessionRow } from "../api.ts";
import { clip, fmtUsd, windowAround } from "../format.ts";
import { type ForestRow, isDelegated } from "../forest.ts";

const KIND_GLYPH: Record<SessionKind, string> = {
  root: "●",
  fork: "⑂",
  compaction: "≣",
  subagent: "◆",
  workflow_agent: "◈",
};

/**
 * Outcome marker. `null` for a session that has never run a turn — an absence, not
 * a state, and rendering a glyph for it would invent one.
 *
 * `outcomeOk === false` is checked ahead of the turn status because it is the
 * DELEGATION outcome (`schema/parts.ts`): a subagent whose turn ended `done` but
 * whose work failed is exactly the branch the tree exists to make findable. There is
 * no acceptance gate behind it — it records that the turn errored, not that a check
 * ran.
 */
export function statusMark(
  s: SessionRow,
  busyBelow = 0,
): { glyph: string; color: string } | null {
  if (s.busy) return { glyph: "⋯", color: "cyan" };
  // Work running UNDER this conversation counts as this conversation running. Without
  // it a root sitting on five live subagents rendered `✓` — the tree saying finished
  // while the rail said "5 agents running".
  if (busyBelow > 0) return { glyph: "⋯", color: "cyan" };
  if (s.outcomeOk === false) return { glyph: "✗", color: "red" };
  switch (s.lastTurnStatus) {
    case "running":
      return { glyph: "⋯", color: "cyan" };
    case "orphaned":
    case "interrupted":
      return { glyph: "◼", color: "yellow" };
    case "error":
      return { glyph: "✗", color: "red" };
    case "done":
      return { glyph: "✓", color: "green" };
    default:
      return null;
  }
}

export function titleOf(s: SessionRow): string {
  // `handoff` belongs in this list and was missing, which was visible: a handoff of a
  // still-untitled conversation is titled `handoff · ` server-side, and the row read
  // `handoff ·` — a prefix with nothing after it. Stripped first, so the fallback can
  // do its job.
  const base = (s.title || "").replace(/^(fork|compacted|handoff|subagent|workflow) · /, "").trim();
  return base || "(untitled)";
}

/** `supervisor` is the agent — the transcript calls it "bough" and so does this. */
const ROLE_LABEL: Record<string, string> = {
  user: "you",
  supervisor: "bough",
  system: "system",
};

const ROLE_COLOR: Record<string, string> = {
  user: "white",
  supervisor: "green",
  system: "yellow",
};

/**
 * The rows on screen, and where the window starts.
 *
 * Exported because `PanelHost` resolves `1`–`9` against the SAME window this
 * paints: two calculations of "which rows are visible" is how a digit comes to
 * select a row nobody can see.
 */
export function forestWindow(
  count: number,
  selected: number,
  rows: number,
  chrome = 0,
): { start: number; height: number } {
  // One row of chrome: the legend, and it goes LAST. It used to be the tab's FIRST
  // row — the only tab that put it there — and the budget reserved four rows for it
  // while flooring the list at three, so a short panel painted rows it did not have
  // and OpenTUI shrank them onto each other (`Panel.tsx`).
  const height = Math.max(0, rows - 1 - chrome);
  const { start, end } = windowAround(selected, count, height);
  const from = Math.max(0, start);
  return { start: from, height: Math.max(0, end - from) };
}

export interface TreeProps {
  rows: readonly ForestRow[];
  selected: number;
  /** The tab body's total row budget, legend and filter row included. */
  height: number;
  /** The `/` buffer, echoed so a narrowed list says what narrowed it. */
  filter?: string;
  filtering?: boolean;
}

export function Tree({ rows: items, selected, height, filter, filtering }: TreeProps) {
  const chrome = filtering || filter ? 1 : 0;
  const { start, height: shown } = forestWindow(items.length, selected, height, chrome);
  const window = items.slice(start, start + shown);
  return (
    <box flexDirection="column">
      {filtering || filter
        ? (
          <text attributes={TextAttributes.DIM} wrapMode="none">
            {`/ ${filter ?? ""}${filtering ? "▌" : ""}`}
          </text>
        )
        : null}
      {items.length === 0
        ? (
          <text attributes={TextAttributes.DIM}>
            {filter ? `nothing matches "${filter}"` : "no conversations yet"}
          </text>
        )
        : null}
      {window.map((item, i) => {
        const idx = start + i;
        const sel = idx === selected;
        const cursor = sel ? "❯ " : "  ";
        const indent = "  ".repeat(item.depth);
        if (item.kind === "collapsed") {
          return (
            <text key={item.id} wrapMode="none">
              <span fg={sel ? "cyan" : undefined}>{cursor}</span>
              <span attributes={TextAttributes.DIM}>
                {`${indent}⋯ ${item.count} delegated · → drill in`}
              </span>
            </text>
          );
        }
        if (item.kind === "message") {
          // A turn: the connector, who said it, the gist. `← active` marks where the
          // next turn would append, which is what makes "go back to here" concrete.
          return (
            <text key={item.id} wrapMode="none">
              <span fg={sel ? "cyan" : undefined}>{cursor}</span>
              <span attributes={TextAttributes.DIM}>{`${indent}${item.last ? "└─ " : "├─ "}`}</span>
              <span fg={ROLE_COLOR[item.role]}>{ROLE_LABEL[item.role] ?? item.role}</span>
              <span attributes={sel ? TextAttributes.BOLD : TextAttributes.NONE}>
                {` ${clip(item.gist, Math.max(12, 54 - item.depth * 2))}`}
              </span>
              {item.active ? <span attributes={TextAttributes.DIM}>{"  ← active"}</span> : null}
            </text>
          );
        }
        const s = item.session;
        const mark = statusMark(s, item.busyBelow);
        return (
          <text key={item.id} wrapMode="none">
            <span fg={sel ? "cyan" : undefined}>{cursor}</span>
            <span>{indent}</span>
            {/* The disclosure comes FIRST and is present on every conversation with
                anything under it: it is the one mark saying this row is a door. */}
            <span attributes={TextAttributes.DIM}>
              {item.expandable ? (item.open ? "▾ " : "▸ ") : "  "}
            </span>
            <span attributes={isDelegated(s.kind) ? TextAttributes.NONE : TextAttributes.DIM}>
              {KIND_GLYPH[s.kind]}
            </span>
            {mark ? <span fg={mark.color}>{` ${mark.glyph}`}</span> : <span>{"  "}</span>}
            <span
              attributes={sel || item.current ? TextAttributes.BOLD : TextAttributes.NONE}
              fg={item.current ? "green" : undefined}
            >
              {` ${clip(titleOf(s), Math.max(12, 46 - item.depth * 2))}`}
            </span>
            {item.delegated > 0
              ? <span attributes={TextAttributes.DIM}>{`  ⋯${item.delegated}`}</span>
              : null}
            {/* Named, not just glyphed: `⋯` says something is live, this says how much
                and is the difference between "look inside" and "leave it alone". */}
            {item.busyBelow > 0
              ? <span fg="cyan">{`  ${item.busyBelow} running`}</span>
              : null}
            {s.costUsd
              ? <span attributes={TextAttributes.DIM}>{`  ${fmtUsd(s.costUsd)}`}</span>
              : null}
          </text>
        );
      })}
      <text attributes={TextAttributes.DIM} wrapMode="none">
        {`${items.length > shown ? `${selected + 1}/${items.length} · ` : ""}` +
          "↑↓ move · →← turns · ⏎ open · ⏎ on a turn forks · / find · esc back"}
      </text>
    </box>
  );
}
