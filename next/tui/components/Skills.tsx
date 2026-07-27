/**
 * The skills tab: the `/name` instruction bundles this install can load (spec §16).
 *
 * THE INVARIANT THIS HOLDS: **an empty list and an absent source are different
 * screens.** Skill discovery is T10.2 and does not exist yet — there is no
 * `skills/` module and no `GET /skills` route — so a host with nothing to hand this
 * component passes `skills: null` with a `note` saying why, and the tab prints that
 * sentence. It does NOT print "no skills installed", which is a claim about the
 * user's `~/.bough/skills` that nothing here has read, and it does not sit on
 * "loading…" forever, which is a lie that looks like a hang. The gap is visible
 * rather than silent, which is the same rule `theme.ts` follows about its missing
 * route.
 *
 * `SkillRow` is declared here rather than imported for the same reason: there is no
 * wire shape to import yet. Only two fields are ever shown and spec §16 fixes both
 * in the frontmatter, so the eventual module can satisfy this without changing it.
 *
 * Split out of `Panel.tsx` so the panel file is chrome and a state machine.
 */
import { Box, Text } from "ink";
import { clip } from "../format.ts";
import { palette } from "../theme.ts";

/** One installed skill, as `SKILL.md` frontmatter carries it (spec §16). */
export interface SkillRow {
  name: string;
  description: string;
}

export interface SkillsTabProps {
  /** `null` = no source. Say why in `note`; never render it as "none installed". */
  skills: SkillRow[] | null;
  rows: number;
  /** Why the list is absent. Shown only when `skills` is null. */
  note?: string;
}

export function SkillsTab({ skills, rows, note }: SkillsTabProps) {
  if (!skills) {
    return note
      ? <Text color={palette.warn} wrap="wrap">{note}</Text>
      : <Text dimColor>loading…</Text>;
  }
  if (skills.length === 0) return <Text dimColor>no skills installed</Text>;
  const height = Math.max(3, rows - 6);
  return (
    <Box flexDirection="column">
      {skills.slice(0, height).map((s) => (
        <Text key={s.name} wrap="truncate">
          <Text bold color={palette.accent}>/{s.name}</Text>
          <Text dimColor>{"  "}{clip(s.description, 60)}</Text>
        </Text>
      ))}
      {skills.length > height ? <Text dimColor>… {skills.length - height} more</Text> : null}
      <Text dimColor wrap="truncate">name a skill in your message to load it</Text>
    </Box>
  );
}
