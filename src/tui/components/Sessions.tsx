/**
 * The sessions tab: a flat, filterable list of every session you can open.
 *
 * THE INVARIANT THIS HOLDS: **visibility is derived, and there is nothing to hide.**
 * Spec §4 and §17 are blunt about it — `subagent` and `workflow_agent` collapse under
 * their origin and there is no archive, deprecate, hide or purge action. So the port
 * from `src/tui/components/SessionPicker.tsx` drops `showDeprecated`, the `x archive`
 * and `D deprecate` bindings, the strikethrough row, and the `u restore` hint. The
 * fields those read (`archivedAt`, `deprecatedAt`) do not exist on `SessionRow`; an
 * affordance for state the system does not have is a promise the server will break.
 *
 * WHY THIS IS A LIST AND `Tree.tsx` IS A TREE. They answer different questions and
 * both are tabs. The tree answers "what came from what" — lineage, drill-in, collapsed
 * fan-outs. This answers "where is the thing I was working on", which is a *search*:
 * one flat list, newest first, narrowed by a fuzzy filter over title and project. The
 * old picker tried to be both and had to draw connector glyphs through a filtered set,
 * where a matched child hangs off a parent that was filtered away.
 *
 * PURE CORE. `sessionItems` is a fold over rows — filter, rank, order — with no React,
 * no clock read and no I/O, so the ordering and matching rules are tested by handing it
 * fixtures. The component windows that list around the cursor and paints it; `now` is a
 * prop because `relTime` takes one (plan §0: the clock is injected, never global).
 */
import { Box, Text } from "ink";
import type { SessionKind } from "../../schema/parts.ts";
import type { SessionRow } from "../api.ts";
import { clip, fuzzyScore, relTime, sessionLabel, windowAround } from "../format.ts";
import { palette } from "../theme.ts";

// ---------------------------------------------------------------------------
// Selection (pure)
// ---------------------------------------------------------------------------

/** Kinds that are conversations you open. Delegated work lives in the tree tab. */
const LISTED_KINDS: readonly SessionKind[] = ["root", "fork", "compaction"];

export interface SessionItem {
  session: SessionRow;
  /** Title with the auto-generated kind prefix removed, or the workspace basename. */
  label: string;
  /** Project directory basename — two sessions on different projects read alike. */
  project: string | null;
}

/** Strip the prefix the server puts on a branch's auto title so rows stay scannable. */
export function labelFor(s: SessionRow): string {
  const stripped = (s.title || "").replace(/^(fork|compacted|subagent|workflow) · /, "");
  return sessionLabel(stripped, s.workspace);
}

/**
 * The visible rows.
 *
 * Order is newest-first and does NOT re-sort by match score: the list is a place, and
 * a cursor that jumps to a different row because one more character changed the
 * ranking is a list you cannot walk. The filter subtracts rows, it never reorders them.
 */
export function sessionItems(rows: readonly SessionRow[], filter = ""): SessionItem[] {
  const q = filter.trim();
  return rows
    .filter((s) => LISTED_KINDS.includes(s.kind))
    .filter((s) => q === "" || matches(s, q))
    .sort((a, b) => b.createdAt - a.createdAt)
    .map((s) => ({
      session: s,
      label: labelFor(s),
      project: s.originDir?.split("/").filter(Boolean).pop() ?? null,
    }));
}

/** Fuzzy over the title AND the project path — you remember one or the other. */
function matches(s: SessionRow, query: string): boolean {
  const haystacks = [labelFor(s), s.originDir ?? "", s.workspace ?? ""];
  return haystacks.some((h) => fuzzyScore(h, query) > 0);
}

const KIND_GLYPH: Record<string, string> = {
  root: "●",
  fork: "⑂",
  compaction: "≣",
};

/** Live/last-run marker. `null` for a session that never ran — an absence, not a state. */
export function runMark(s: SessionRow): { glyph: string; color: string } | null {
  if (s.busy) return { glyph: "⋯", color: palette.warn };
  switch (s.lastTurnStatus) {
    case "running":
      return { glyph: "⋯", color: palette.warn };
    case "interrupted":
    case "orphaned":
      return { glyph: "◼", color: palette.warn };
    case "error":
      return { glyph: "✗", color: palette.error };
    case "done":
      return { glyph: "✓", color: palette.accent };
    default:
      return null;
  }
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

export interface SessionsProps {
  items: SessionItem[];
  selected: number;
  /** The open session — tagged "here" rather than given a cursor glyph of its own. */
  currentId?: string | null;
  /** The filter text. `filtering` puts `/`-entry in the row above the list. */
  filter?: string;
  filtering?: boolean;
  rows: number;
  /** Injected clock for `relTime`. */
  now: number;
  /** Transient feedback (why a keypress did not apply). */
  message?: string | null;
}

export function Sessions(
  { items, selected, currentId, filter = "", filtering = false, rows, now, message }: SessionsProps,
) {
  // Chrome above and below the list: the filter line, the legend, the hint.
  const height = Math.max(3, rows - 6);
  const { start, end } = windowAround(selected, items.length, height);
  const window = items.slice(Math.max(0, start), end);
  return (
    <Box flexDirection="column">
      {message ? <Text color={palette.warn} wrap="truncate">{message}</Text> : null}
      {filtering
        ? (
          <Text>
            <Text color={palette.accent}>{"/ "}</Text>
            {filter}
            <Text inverse>{" "}</Text>
          </Text>
        )
        : filter
        ? <Text dimColor>/ {filter}</Text>
        : null}
      {items.length === 0
        ? <Text dimColor>{filter ? "nothing matches that filter" : "no sessions yet"}</Text>
        : null}
      {window.map((item, i) => {
        const idx = Math.max(0, start) + i;
        const sel = idx === selected;
        const s = item.session;
        const here = s.id === currentId;
        const mark = runMark(s);
        return (
          <Text key={s.id} wrap="truncate">
            <Text color={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</Text>
            <Text dimColor={s.kind !== "root"}>{KIND_GLYPH[s.kind] ?? "•"}</Text>
            {mark
              ? <Text color={sel ? undefined : mark.color}>{` ${mark.glyph}`}</Text>
              : <Text>{"  "}</Text>}
            <Text bold={here}>{" "}{clip(item.label, 46)}</Text>
            {item.project ? <Text dimColor>{"  "}{item.project}</Text> : null}
            <Text dimColor>{"  "}{here ? "here · " : ""}{relTime(s.createdAt, now)}</Text>
          </Text>
        );
      })}
      {end < items.length || start > 0
        ? <Text dimColor>— {Math.max(0, start) + window.length}/{items.length} —</Text>
        : null}
      <Text dimColor wrap="truncate">● root · ⑂ fork · ≣ compacted · ⋯ running · ✗ failed</Text>
      <Text dimColor wrap="truncate">↑↓ move · ⏎ open · / filter · n new</Text>
    </Box>
  );
}
