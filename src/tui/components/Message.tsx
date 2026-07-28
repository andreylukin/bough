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
 * exact display width with `truncateAnsi` before the renderer sees the string, so a line
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
import { RGBA, StyledText, type TextChunk, TextAttributes } from "@opentui/core";
import type { Message } from "../../schema/parts.ts";
import { messageLines, type VLine } from "../lines.ts";
import { ansiSpans, truncateAnsi, width as displayWidth } from "../format.ts";

/**
 * Pad to exactly `w` display columns.
 *
 * NOT cosmetic — this is what CLEARS the row. A `<text>` node is content-sized, so
 * a short line drawn over a longer previous one repaints only its own cells and
 * the tail of the old line survives underneath: `bough` painted over
 * `  hello world▌` rendered as `boughlo world▌`. Every row this file emits is
 * blanked to the full viewport width before the renderer sees it.
 */
export function padRow(text: string, w: number): string {
  // A newline inside a ROW is a frame-wide defect, not a cosmetic one: every surface
  // here reserves a fixed number of rows and computes the ones below it by
  // subtraction, so one string that paints two rows pushes the composer and the
  // status line off theirs and the screen comes apart. It reached the rail through a
  // multi-line shell command. Sources sanitize deliberately (`oneLine`, which marks
  // the join with `¶`); this is the backstop that makes it impossible, and it only
  // touches line breaks — collapsing runs of spaces here would eat the column
  // alignment every caller builds before it.
  const flat = text.replace(/\r?\n/g, " ");
  return flat + " ".repeat(Math.max(0, w - displayWidth(flat)));
}

/**
 * A styled string as the renderer wants it: chunks, not escapes.
 *
 * THE OTHER HALF OF THE ROW-CLEARING FIX, and the one that actually mattered. A
 * `<text>` whose child carries raw SGR escapes desynchronises after its first
 * repaint — the cell diff and the escape run disagree about which column is
 * which, so a redrawn row keeps pieces of the row that was there before, escape
 * tails included. Proven both ways in a bare OpenTUI app: identical content,
 * corrupt as a string, clean as chunks. `ansiSpans` (format.ts) does the parsing;
 * this only maps a span onto OpenTUI's `TextChunk`.
 */
export function styledRow(text: string): StyledText {
  const chunks: TextChunk[] = ansiSpans(text).map((s) => {
    let attributes = 0;
    if (s.bold) attributes |= TextAttributes.BOLD;
    if (s.dim) attributes |= TextAttributes.DIM;
    if (s.italic) attributes |= TextAttributes.ITALIC;
    if (s.underline) attributes |= TextAttributes.UNDERLINE;
    if (s.reverse) attributes |= TextAttributes.INVERSE;
    if (s.strikethrough) attributes |= TextAttributes.STRIKETHROUGH;
    return {
      __isChunk: true,
      text: s.text,
      fg: s.fg ? RGBA.fromHex(s.fg) : undefined,
      bg: s.bg ? RGBA.fromHex(s.bg) : undefined,
      attributes,
      link: s.link ? { url: s.link } : undefined,
    };
  });
  // A chunkless StyledText renders nothing at all, which would drop the row.
  return new StyledText(chunks.length ? chunks : [{ __isChunk: true, text: " " }]);
}

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
  // A blank line must still occupy its row, so an empty string is drawn as a space
  // rather than handed to the renderer as nothing.
  const raw = line.text === "" ? " " : line.text;
  const w = Math.max(1, width);
  const text = truncateAnsi(decorate ? decorate(raw) : raw, w);
  return <text wrapMode="none" content={styledRow(padRow(text || " ", w))} />;
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
    <box flexDirection="column">
      {lines.map((line, i) => <MessageRow key={i} line={line} width={width} />)}
    </box>
  );
}
