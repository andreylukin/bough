import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { AskQuestion } from "../../schema/parts.ts";
import { ASK_OPTIONS_HINT, ASK_TYPING_BACK_HINT, ASK_TYPING_HINT } from "../keys.ts";

// The question-hold card for ask() (asks.ts): a program is
// parked mid-turn on a question to the human, so the card replaces the composer
// until it's answered or declined. Number keys pick an option; `t` (or an
// option-less question, which starts there) switches to the free-text line.
export function AskCard(
  { q, count, input, typing }: {
    q: AskQuestion;
    count: number;
    /** The free-text draft (App owns the buffer, like the composer's). */
    input: string;
    /** True while the free-text line is active (always, when there are no options). */
    typing: boolean;
  },
) {
  const options = q.options ?? [];
  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      backgroundColor={palette.panel}
      borderColor={palette.warn}
      paddingX={1}
    >
      <Text color={palette.warn} bold>
        ? question{count > 1 ? ` (1 of ${count})` : ""}
      </Text>
      <Text wrap="wrap">{q.question}</Text>
      {options.map((opt, i) => (
        <Text key={i} wrap="truncate">
          {"  "}
          <Text color={palette.accent} bold>{i + 1}</Text> {opt}
        </Text>
      ))}
      {typing
        ? (
          <Text wrap="truncate">
            <Text dimColor>{"> "}</Text>
            {input}
            <Text color={palette.accent}>▌</Text>
          </Text>
        )
        : null}
      <Text dimColor>
        {typing ? (options.length ? ASK_TYPING_BACK_HINT : ASK_TYPING_HINT) : ASK_OPTIONS_HINT}
      </Text>
    </Box>
  );
}
