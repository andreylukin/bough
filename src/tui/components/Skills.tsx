/**
 * The skills tab: the `/name` instruction bundles this install can load (spec §16).
 *
 * THE INVARIANT THIS HOLDS: **an empty list, an absent source and a BROKEN skill are
 * three different screens.** `null` means nothing has answered yet or the fetch
 * failed — say why in `note`, and never render it as "no skills installed", which is
 * a claim about the user's `~/.bough/skills` that this component has not read. An
 * empty array IS that claim, and it only becomes safe to make once `GET /skills` has
 * answered; the `sources` it rides along with are printed beside it, because "why is
 * my skill not listed?" is almost always answered by naming the directory that was
 * walked.
 *
 * The third case is the one worth a component: a skill whose SKILL.md could not be
 * parsed is served WITH its `error` rather than omitted (`server/skills.ts`), so it
 * is rendered here in the error colour with the reason. A malformed skill that simply
 * vanished from the list would instead be discovered as a `/name` that quietly did
 * nothing, two turns and some money later.
 *
 * The row shape is IMPORTED from `server/skills.ts` rather than restated. It used to
 * be declared here, correctly, because T10.2 had not landed and there was no wire
 * shape to import; that gap is closed, and a local copy would now be a second
 * definition free to drift from what the route actually serves.
 *
 * Split out of `Panel.tsx` so the panel file is chrome and a state machine.
 */
import { TextAttributes } from "@opentui/core";
import { clip, legendLine } from "../format.ts";
import { palette } from "../theme.ts";
// Type-only: erased at compile time, so this component keeps its no-server-imports
// property and cannot drag a handler module into the render graph.
import type { SkillRow } from "../../server/skills.ts";

export type { SkillRow };

/** Where the listing was read from — `GET /skills` returns these beside the rows. */
export interface SkillSourceRow {
  source: string;
  dir: string;
}

/**
 * The visible slice, sized from what is left after the chrome.
 *
 * `Math.max(3, rows - 6)` claimed three list rows at any height, so a short panel
 * painted more rows than it had and OpenTUI shrank them onto each other
 * (`Panel.tsx`). Chrome here is the counter row, the `read from …` line and the
 * legend — the legend is the last row of the tab and never gives up its place.
 *
 * Exported so `PanelHost` resolves `1`–`9` against the rows actually on screen.
 */
export function skillsWindow(
  count: number,
  selected: number,
  rows: number,
  chrome = 0,
): { start: number; height: number; counter: boolean } {
  const avail = Math.max(0, rows - chrome - 1 /* legend */);
  // Content over indicators when it is tight: a lone `1/40` row above no skills at
  // all is a position report about a list nobody can see.
  const counter = count > avail && avail >= 2;
  const height = Math.max(0, avail - (counter ? 1 : 0));
  const at = Math.max(0, Math.min(selected, count - 1));
  const start = Math.max(0, Math.min(at - Math.floor(height / 2), count - height));
  return { start, height, counter };
}

export interface SkillsTabProps {
  /** Cursor row. The panel already moves it for this tab; nothing drew it. */
  selected?: number;
  /**
   * Columns available. The description used to be clipped at a hardcoded 60
   * characters, so at 200 columns a skill's description still cut off at column
   * 80 with 120 blank columns beside it — and there is no way to read the rest.
   */
  cols?: number;
  /** `null` = nothing has answered yet. Say why in `note`; never fake an empty list. */
  skills: SkillRow[] | null;
  rows: number;
  /** Why the list is absent. Shown only when `skills` is null. */
  note?: string;
  /** The directories that were walked. Printed so an empty list is diagnosable. */
  sources?: readonly SkillSourceRow[];
  /** The `/` filter buffer. Narrowing happens in `PanelHost`; this only draws it. */
  filter?: string;
  filtering?: boolean;
}

export function SkillsTab(
  { skills, rows, cols, selected = 0, note, sources, filter = "", filtering = false }:
    SkillsTabProps,
) {
  if (!skills) {
    return note
      ? <text fg={palette.warn} wrapMode="word">{note}</text>
      : <text attributes={TextAttributes.DIM}>loading…</text>;
  }
  const where = sources && sources.length > 0
    ? sources.map((s) => `${s.source} ${s.dir}`).join(" · ")
    : null;
  const legend = (
    <text attributes={TextAttributes.DIM} wrapMode="none">
      {filtering
        ? "type to narrow · ⌫ back · esc clear the filter · ↑↓ move"
        : legendLine([
          "↑↓ move",
          "pgup/pgdn page",
          "1-9 pick",
          "/ filter",
          "name a skill to load it",
          "esc back",
        ], cols)}
    </text>
  );
  if (skills.length === 0) {
    return (
      <box flexDirection="column">
        <text attributes={TextAttributes.DIM}>
          {filter ? "nothing matches that filter" : "no skills installed"}
        </text>
        {where
          ? <text attributes={TextAttributes.DIM} wrapMode="none">read from {where}</text>
          : null}
        {legend}
      </box>
    );
  }
  const chrome = (filtering || filter ? 1 : 0) + (where ? 1 : 0);
  // A WINDOW around the cursor, not the first N rows. The panel has always moved
  // `sel` for this tab — the row count is in its table — but the list drew from
  // index 0 with no marker, so ↑↓ and ⏎ were documented by the panel and inert
  // here, and a skill past the fold could not be reached or read at all.
  const { start, height, counter } = skillsWindow(skills.length, selected, rows, chrome);
  const at = Math.max(0, Math.min(selected, skills.length - 1));
  const window = height === 0 ? [] : skills.slice(start, start + height);
  return (
    <box flexDirection="column">
      {filtering
        ? (
          <text>
            <span fg={palette.accent}>{"/ "}</span>
            {filter}
            <span fg="black" bg={palette.accent}>{" "}</span>
          </text>
        )
        : filter
        ? <text attributes={TextAttributes.DIM}>/ {filter}</text>
        : null}
      {window.map((s, i) => (
        <text
          key={s.name}
          wrapMode="none"
        >
          <span attributes={TextAttributes.DIM}>{i < 9 ? `${i + 1} ` : "  "}</span>
          {/* The `❯` carries the cursor on its own. INVERSE renders invisible after
              the OpenTUI migration (white-on-white — see `CARET_FG` in
              `Composer.tsx`), so the marked row was marked by a dim chevron and
              nothing else. */}
          <span fg={start + i === at ? palette.accent : undefined} attributes={TextAttributes.DIM}>
            {start + i === at ? "❯ " : "  "}
          </span>
          <span
            attributes={TextAttributes.BOLD}
            fg={s.error ? palette.error : palette.accent}
          >
            /{s.name}
          </span>
          <span attributes={TextAttributes.DIM}>
            {"  "}
            {clip(s.error ?? s.description, Math.max(20, (cols ?? 80) - s.name.length - 8))}
          </span>
          {s.mcp && s.mcp.length > 0
            ? <span fg={palette.info}>{"  mcp: " + s.mcp.join(", ")}</span>
            : null}
        </text>
      ))}
      {counter
        ? (
          <text attributes={TextAttributes.DIM}>
            {`${at + 1}/${skills.length} · ↑↓ to see the rest`}
          </text>
        )
        : null}
      {where
        ? <text attributes={TextAttributes.DIM} wrapMode="none">read from {where}</text>
        : null}
      {/* Last row, like every other tab. `read from …` used to sit here, so the one
          place a reader learns to look for keys held a directory listing instead. */}
      {legend}
    </box>
  );
}
