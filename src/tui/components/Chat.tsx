/**
 * The chat view: a virtualized window over the pre-wrapped transcript, plus the
 * two facts that must never scroll away — what the agent is doing right now, and
 * what it is costing.
 *
 * THE INVARIANT THIS HOLDS: **presentational only.** Every value shown is a prop.
 * This component fetches nothing, holds no transcript state, and derives no
 * `VLine`s of its own — the caller builds them with `buildLines` because it needs
 * that same array for click hit-testing and search, and building it twice would be
 * two sources of truth for which row is which (plan M9: "no monolithic
 * component"). Scrolling is likewise a prop: `scrollOff` counts lines up from the
 * live tail, so growth at the bottom never drags the reading position.
 *
 * SECOND INVARIANT — **the transcript hangs from the bottom.** A short
 * conversation is padded above, not below, so the newest line sits where the eye
 * already is and the composer never jumps up the screen as the first reply
 * arrives.
 *
 * THIRD — **cost and context are chrome, not a panel** (spec §15: chat shows live
 * cost and context). They render on one dim strip under the transcript, so a run
 * that is quietly eating the context window is visible without opening anything.
 * The numbers are formatted by `format.ts`; an unknown context limit shows tokens
 * rather than an invented percentage.
 */
import { Box, Text } from "ink";
import type { VLine } from "../lines.ts";
import { visibleSlice } from "../lines.ts";
import { busyLine, meterLine, UI } from "../format.ts";
import { MessageRow } from "./Message.tsx";

export interface ChatMeter {
  model?: string | null;
  costUsd?: number | null;
  contextTokens?: number | null;
  contextLimit?: number | null;
  /** Where the turn runs, already shortened. Leads the line — see `meterLine`. */
  workspace?: string | null;
  /** Append the `? help` hint. The chat sets it; other surfaces need not. */
  help?: boolean;
  /** A cold-cache or disconnect note from `format.ts`. Rendered as-is. */
  note?: string | null;
  /** The note is a problem, not an aside. */
  noteUrgent?: boolean;
}

export interface ChatProps {
  /** The whole transcript, pre-wrapped. Built by the caller with `buildLines`. */
  lines: VLine[];
  width: number;
  /** Rows the transcript body may occupy. */
  height: number;
  /** Lines up from the live tail. 0 = pinned to the bottom, following output. */
  scrollOff?: number;
  meter?: ChatMeter;
  /**
   * The cheap-tier activity blurb for the running turn. Absent is the normal
   * case — every cheap-tier feature fails silently (spec §12), so this line is
   * never load-bearing.
   */
  activity?: string | null;
  /** A turn is in flight. Drives the spinner line — see `busyLine`. */
  busy?: boolean;
  /** How long the running turn has been going. Ignored unless `busy`. */
  elapsedMs?: number;
  /** Spinner phase. The caller owns the clock so this stays render-pure. */
  tick?: number;
  /** Messages typed while a turn ran, held locally until it drains (spec §5). */
  queued?: string[];
  /** A transient message (a copy, an error). */
  notice?: string | null;
  /**
   * Applied to a row's finished text — search marks, a drag selection. Gets the
   * text, the line it came from and its index in `lines`, because a highlight is
   * addressed by transcript position, not by viewport row.
   */
  decorate?: (text: string, line: VLine, index: number) => string;
  /** Shown instead of the transcript when the session has no messages yet. */
  placeholder?: string;
  /**
   * The second empty-state line: what bough will actually do to this machine.
   *
   * Spec §2 says bough "states this plainly rather than implying safety it does
   * not provide", and the README leads with it in bold — but the app said it in
   * exactly one place, forty-odd rows down the `?` overlay under "won't do". A
   * cautious first-time user pressed ↓ eighteen times to find out that their files
   * get edited without confirmation, which is not stating it plainly. The empty
   * state is the one screen that exists BEFORE anything has happened, and it was
   * seventeen blank rows.
   */
  posture?: string;
}

export function Chat(
  {
    lines,
    width,
    height,
    scrollOff = 0,
    meter,
    activity,
    busy = false,
    elapsedMs = 0,
    tick = 0,
    queued = [],
    notice,
    decorate,
    placeholder = "type to start · the agent writes one program per round",
    posture = "it runs as you, with your authority — no sandbox, and edits land in your files",
  }: ChatProps,
) {
  const body = Math.max(1, height);
  const { start, rows, more, pct } = visibleSlice(lines, body, scrollOff);
  // Pad above, never below: the newest line stays where the eye already is.
  const pad = Math.max(0, body - rows.length);
  const meterText = meter ? meterLine({ ...meter, width }) : "";
  return (
    <Box flexDirection="column" width={width}>
      <Box flexDirection="column" flexGrow={1}>
        {lines.length === 0
          ? (
            <>
              {Array.from(
                { length: Math.max(0, body - 2) },
                (_v, i) => <Text key={`pad-${i}`}>{" "}</Text>,
              )}
              <Text dimColor wrap="truncate">{placeholder}</Text>
              <Text color={UI.warn} wrap="truncate">{posture}</Text>
            </>
          )
          : (
            <>
              {Array.from({ length: pad }, (_v, i) => <Text key={`pad-${i}`}>{" "}</Text>)}
              {rows.map((line, i) => (
                <MessageRow
                  key={`l-${start + i}`}
                  line={line}
                  width={width}
                  decorate={decorate && ((text) => decorate(text, line, start + i))}
                />
              ))}
            </>
          )}
      </Box>
      {more > 0
        ? (
          // Chrome, not content: the arrow and count carry the emphasis. The
          // percentage is the viewport TOP's position in the thread, so fully
          // scrolled up reads 0%.
          <Text>
            <Text color={UI.info}>↓ {more}</Text>
            <Text dimColor>{" "}more line{more === 1 ? "" : "s"} below · {pct}%</Text>
          </Text>
        )
        : null}
      {queued.map((q, i) => <Text key={`q-${i}`} dimColor wrap="truncate">⧖ queued: {q}</Text>)}
      {busy
        ? (
          // While a turn runs this REPLACES the bare activity blurb: it carries the
          // blurb's text when there is one, and says "working" when there is not,
          // so the running state is never a blank screen.
          <Text wrap="truncate">
            <Text color={UI.accent}>{busyLine({ activity, elapsedMs, tick }).slice(0, 2)}</Text>
            <Text dimColor>{busyLine({ activity, elapsedMs, tick }).slice(2)}</Text>
          </Text>
        )
        : activity
        ? (
          <Text wrap="truncate">
            <Text color={UI.accent}>{"⋯ "}</Text>
            <Text dimColor>{activity}</Text>
          </Text>
        )
        : null}
      {notice ? <Text color={UI.warn} wrap="truncate">{notice}</Text> : null}
      {meterText || meter?.note
        ? (
          <Text wrap="truncate">
            {meterText ? <Text dimColor>{meterText}</Text> : null}
            {meter?.note
              ? (
                <Text color={meter.noteUrgent ? UI.error : undefined} dimColor={!meter.noteUrgent}>
                  {meterText ? "  " : ""}
                  {meter.note}
                </Text>
              )
              : null}
          </Text>
        )
        : null}
    </Box>
  );
}
