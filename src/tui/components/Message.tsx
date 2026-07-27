/**
 * One message, and one rendered row of one.
 *
 * THE INVARIANT THIS HOLDS: **presentational only — props in, nothing out.** These
 * components fetch nothing, subscribe to nothing, and mutate no store; every fact
 * they show arrives as a prop and every fold decision arrives as a predicate. That
 * is what lets them be rendered from a fixture with no server and no terminal
 * (plan M9), and it is the boundary the previous tree's 3,618-line `App.tsx`
 * lacked.
 *
 * SECOND INVARIANT — **a row is one terminal row.** `MessageRow` truncates to the
 * exact display width with `truncateAnsi` before Ink sees the string, so a line
 * carrying SGR escapes, an OSC 8 hyperlink or a wide CJK glyph occupies the same
 * one row as a plain one. Measuring with `String.length` would let a styled line
 * reflow and shove the whole viewport down by a row per message.
 *
 * Two exports, because the transcript needs both shapes of the same thing:
 * `MessageRow` renders a pre-wrapped `VLine` — what the virtualized chat viewport
 * paints — and `MessageView` renders a whole message standalone, for the surfaces
 * that show one message outside a viewport (a branch drill-in, a fixture test).
 * Both go through `lines.ts`, so the folding rules have exactly one definition.
 */
import { Box, Text } from "ink";
import type { Message } from "../../schema/parts.ts";
import { messageLines, type VLine } from "../lines.ts";
import { truncateAnsi } from "../format.ts";

export interface MessageRowProps {
  line: VLine;
  /** Display columns available. The row is truncated to exactly this. */
  width: number;
  /**
   * Optional decoration applied to the finished text — search marks, a drag
   * selection. Passed in rather than computed here: the highlight depends on
   * viewport state this component deliberately does not know about.
   */
  decorate?: (text: string) => string;
}

export function MessageRow({ line, width, decorate }: MessageRowProps) {
  // A blank line must still occupy its row: Ink collapses an empty string.
  const raw = line.text === "" ? " " : line.text;
  const text = truncateAnsi(decorate ? decorate(raw) : raw, Math.max(1, width));
  return <Text wrap="truncate">{text || " "}</Text>;
}

export interface MessageViewProps {
  message: Message;
  width: number;
  /** Which fold keys are open. Caller state — see `lines.ts`, second invariant. */
  isExpanded?: (key: string) => boolean;
  /** Which truncated blocks have had their line cap lifted. */
  isFull?: (key: string) => boolean;
  /** Text streamed for this message that has not landed as a part yet. */
  streaming?: string;
  /** callId → live `console.*` lines from the running program. */
  toolLogs?: Record<string, string[]>;
}

const CLOSED = () => false;

export function MessageView(
  { message, width, isExpanded = CLOSED, isFull = CLOSED, streaming, toolLogs }: MessageViewProps,
) {
  const lines = messageLines(message, isExpanded, isFull, width, streaming, toolLogs);
  return (
    <Box flexDirection="column">
      {lines.map((line, i) => <MessageRow key={i} line={line} width={width} />)}
    </Box>
  );
}
