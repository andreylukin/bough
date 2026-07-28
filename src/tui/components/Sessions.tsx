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
import { TextAttributes } from "@opentui/core";
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

/**
 * THE ROW BUDGET, EXACT — and it is a shared function because it was neither.
 *
 * The tab used to reserve a flat six rows for chrome and then floor the list at three
 * (`Math.max(3, rows - 6)`), so at twelve terminal rows it painted six rows into
 * three and OpenTUI shrank them onto each other (see `Panel.tsx`). Every row the
 * component can emit is counted here instead: the message, the filter line, the
 * `— n/m —` counter, and the two legend rows. Whatever is left is the list, and when
 * nothing is left the list is EMPTY rather than three rows deep — the legend is the
 * last row of the tab and it is the one thing that may not be squeezed out.
 *
 * It is EXPORTED because `PanelHost` needs the same window the body draws: `1`–`9`
 * address rows on screen, so the host and the body disagreeing about which rows are
 * on screen would make a digit land somewhere the user cannot see.
 */
export function sessionsWindow(
  count: number,
  selected: number,
  rows: number,
  chrome = 0,
): { start: number; end: number; height: number; glyphKey: boolean; counter: boolean } {
  // WHEN IT IS TIGHT, CONTENT WINS. The glyph key is a decoder ring and the `— n/m —`
  // counter is a position report; a panel that spends its last two rows on those and
  // shows no sessions at all has answered a question nobody asked. The key legend is
  // never dropped — it is the only row that says how to get out.
  const glyphKey = rows - chrome >= 5;
  const avail = Math.max(0, rows - chrome - (glyphKey ? 2 : 1));
  const counter = count > avail && avail >= 2;
  const height = Math.max(0, avail - (counter ? 1 : 0));
  const { start, end } = windowAround(selected, count, height);
  return { start: Math.max(0, start), end, height, glyphKey, counter };
}

export function Sessions(
  { items, selected, currentId, filter = "", filtering = false, rows, now, message }: SessionsProps,
) {
  const chrome = (message ? 1 : 0) + (filtering || filter ? 1 : 0);
  const { start, end, height, glyphKey, counter } = sessionsWindow(
    items.length,
    selected,
    rows,
    chrome,
  );
  const window = height === 0 ? [] : items.slice(start, end);
  return (
    <box flexDirection="column">
      {message ? <text fg={palette.warn} wrapMode="none">{message}</text> : null}
      {filtering
        ? (
          <text>
            <span fg={palette.accent}>{"/ "}</span>
            {filter}
            {/* An explicit pair, not INVERSE: OpenTUI double-signals reverse video
                (it writes a white background AND leaves SGR 7 set), so the caret
                came out white-on-white — see `CARET_FG` in `Composer.tsx`. */}
            <span fg="black" bg={palette.accent}>{" "}</span>
          </text>
        )
        : filter
        ? <text attributes={TextAttributes.DIM}>/ {filter}</text>
        : null}
      {items.length === 0 && height > 0
        ? (
          <text attributes={TextAttributes.DIM}>
            {filter ? "nothing matches that filter" : "no sessions yet"}
          </text>
        )
        : null}
      {window.map((item, i) => {
        const idx = Math.max(0, start) + i;
        const sel = idx === selected;
        const s = item.session;
        const here = s.id === currentId;
        const mark = runMark(s);
        return (
          <text key={s.id} wrapMode="none">
            {/* The row's own number, so `1`–`9` is a visible affordance and not a
                shortcut you have to be told about. It addresses the row's position
                ON SCREEN, which is why it is drawn from the window index. */}
            <span attributes={TextAttributes.DIM}>{i < 9 ? `${i + 1} ` : "  "}</span>
            <span fg={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</span>
            <span attributes={s.kind === "root" ? TextAttributes.NONE : TextAttributes.DIM}>
              {KIND_GLYPH[s.kind] ?? "•"}
            </span>
            {mark
              ? <span fg={sel ? undefined : mark.color}>{` ${mark.glyph}`}</span>
              : <span>{"  "}</span>}
            <span attributes={here ? TextAttributes.BOLD : TextAttributes.NONE}>
              {" "}
              {clip(item.label, 46)}
            </span>
            {item.project
              ? <span attributes={TextAttributes.DIM}>{"  "}{item.project}</span>
              : null}
            <span attributes={TextAttributes.DIM}>
              {"  "}
              {here ? "here · " : ""}
              {relTime(s.createdAt, now)}
            </span>
          </text>
        );
      })}
      {counter
        ? (
          <text attributes={TextAttributes.DIM}>
            — {Math.max(0, start) + window.length}/{items.length} —
          </text>
        )
        : null}
      {glyphKey
        ? (
          <text attributes={TextAttributes.DIM} wrapMode="none">
            ● root · ⑂ fork · ≣ compacted · ⋯ running · ✗ failed
          </text>
        )
        : null}
      {
        /*
        Only keys the keymap actually binds, and it is the LAST row of the tab —
        the same position on every tab, so the answer to "what can I press here"
        is always in the same place. It used to advertise "/ filter · n new" when
        `keys.ts` bound neither; `/` is bound now (`panel.filter`, scoped to
        `FILTER_TABS`) and `n` still is not, so `n` is still not named.
      */
      }
      <text attributes={TextAttributes.DIM} wrapMode="none">
        {filtering
          ? "type to narrow · ⌫ back · esc clear the filter · ↑↓ move · ⏎ open"
          : "↑↓ move · pgup/pgdn page · 1-9 pick · / filter · ⏎ open · → drill in · esc back"}
      </text>
    </box>
  );
}
