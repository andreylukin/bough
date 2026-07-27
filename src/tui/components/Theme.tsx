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
import { Box, Text } from "ink";
import { windowAround } from "../format.ts";
import { palette, presetSwatch, type ThemePreview } from "../theme.ts";

export interface ThemeTabProps {
  preview: ThemePreview | null;
  rows: number;
}

export function ThemeTab({ preview, rows }: ThemeTabProps) {
  if (!preview) return <Text dimColor>loading theme…</Text>;
  const height = Math.max(3, rows - 5);
  const { start, end } = windowAround(preview.index, preview.presets.length, height);
  return (
    <Box flexDirection="column">
      <Text dimColor wrap="truncate">
        {preview.previewing ? "previewing " : "current: "}
        {preview.name} — ↑↓ preview live · ⏎ keep · leaving the tab reverts
      </Text>
      {preview.presets.slice(Math.max(0, start), end).map((p, i) => {
        const sel = Math.max(0, start) + i === preview.index;
        return (
          <Box key={p.name}>
            <Text wrap="truncate">
              <Text color={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</Text>
              <Text bold={sel}>{p.name.padEnd(16)}</Text>
            </Text>
            <Text wrap="truncate">
              {presetSwatch(p).map((cell) => (
                <Text key={cell.token} color={cell.color}>{cell.block}</Text>
              ))}
            </Text>
            <Text dimColor wrap="truncate">{"  "}{p.note}</Text>
          </Box>
        );
      })}
    </Box>
  );
}
