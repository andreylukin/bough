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
import { Box, Text } from "ink";
import { clip } from "../format.ts";
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

export interface SkillsTabProps {
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
}

export function SkillsTab({ skills, rows, cols, note, sources }: SkillsTabProps) {
  if (!skills) {
    return note
      ? <Text color={palette.warn} wrap="wrap">{note}</Text>
      : <Text dimColor>loading…</Text>;
  }
  const where = sources && sources.length > 0
    ? sources.map((s) => `${s.source} ${s.dir}`).join(" · ")
    : null;
  if (skills.length === 0) {
    return (
      <Box flexDirection="column">
        <Text dimColor>no skills installed</Text>
        {where ? <Text dimColor wrap="truncate">read from {where}</Text> : null}
      </Box>
    );
  }
  const height = Math.max(3, rows - 6);
  return (
    <Box flexDirection="column">
      {skills.slice(0, height).map((s) => (
        <Text key={s.name} wrap="truncate">
          <Text bold color={s.error ? palette.error : palette.accent}>/{s.name}</Text>
          <Text dimColor>
            {"  "}
            {clip(s.error ?? s.description, Math.max(20, (cols ?? 80) - s.name.length - 8))}
          </Text>
          {s.mcp && s.mcp.length > 0
            ? <Text color={palette.info}>{"  mcp: " + s.mcp.join(", ")}</Text>
            : null}
        </Text>
      ))}
      {skills.length > height ? <Text dimColor>… {skills.length - height} more</Text> : null}
      <Text dimColor wrap="truncate">name a skill in your message to load it</Text>
      {where ? <Text dimColor wrap="truncate">read from {where}</Text> : null}
    </Box>
  );
}
