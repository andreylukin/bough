/**
 * The theme tab: browse a palette by wearing it.
 *
 * THE INVARIANT THIS HOLDS: **the preview is the product, not a swatch.** Spec §16
 * requires the picker to "preview live on cursor move and revert on exit", so moving
 * the cursor repaints the whole TUI through `theme.ts`'s live `palette`; this file
 * renders rows and nothing else. The revert is not here either — it is in the panel's
 * reducer, because the thing that knows you left a tab is the thing that owns the
 * tabs, and a revert remembered at four of the five exits is a picker that silently
 * keeps the theme you last scrolled past.
 *
 * Each row's swatch is resolved from the PRESET's own colours (`presetSwatch`), never
 * from the live palette, so a row looks like itself whether or not it is the one
 * currently painted.
 *
 * Split out of `Panel.tsx` so the panel file is chrome and a state machine.
 */
import { TextAttributes } from "@opentui/core";
import { windowAround } from "../format.ts";
import { palette, presetSwatch, type ThemePreview } from "../theme.ts";

export interface ThemeTabProps {
  preview: ThemePreview | null;
  rows: number;
}

export function ThemeTab({ preview, rows }: ThemeTabProps) {
  if (!preview) return <text attributes={TextAttributes.DIM}>loading theme…</text>;
  // One row of chrome — the legend — and it is the LAST row, like every other tab.
  // `Math.max(3, rows - 5)` reserved five rows for one and then floored the list at
  // three, which is how a short panel came to paint more rows than it had.
  const height = Math.max(0, rows - 1);
  const { start, end } = windowAround(preview.index, preview.presets.length, height);
  return (
    <box flexDirection="column">
      {(height === 0 ? [] : preview.presets.slice(Math.max(0, start), end)).map((p, i) => {
        const sel = Math.max(0, start) + i === preview.index;
        return (
          // `flexDirection` is spelled out because a box defaults to a COLUMN here,
          // where ink's Box defaulted to a row: the three cells are one row.
          <box key={p.name} flexDirection="row">
            <text wrapMode="none">
              <span fg={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</span>
              <span attributes={sel ? TextAttributes.BOLD : TextAttributes.NONE}>
                {p.name.padEnd(16)}
              </span>
            </text>
            <text wrapMode="none">
              {presetSwatch(p).map((cell) => (
                <span key={cell.token} fg={cell.color}>{cell.block}</span>
              ))}
            </text>
            <text attributes={TextAttributes.DIM} wrapMode="none">{"  "}{p.note}</text>
          </box>
        );
      })}
      <text attributes={TextAttributes.DIM} wrapMode="none">
        {preview.previewing ? "previewing " : "current: "}
        {preview.name} — ↑↓ preview live · ⏎ keep · esc back (leaving reverts)
      </text>
    </box>
  );
}
