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
import { TextAttributes } from "@opentui/core";
import type { VLine } from "../lines.ts";
import { chatBodyHeight, visibleSlice } from "../lines.ts";
import { busyLine, meterLine, UI } from "../format.ts";
import { MessageRow, padRow } from "./Message.tsx";

export interface ChatMeter {
  model?: string | null;
  /** Thinking depth when it is not the default — see `meterLine`. */
  effort?: string | null;
  costUsd?: number | null;
  contextTokens?: number | null;
  contextLimit?: number | null;
  /** Where the turn runs, already shortened. Leads the line — see `meterLine`. */
  workspace?: string | null;
  /** The branch those edits land on. */
  branch?: string | null;
  /** Background shells still running — see `meterLine`. */
  shells?: number | null;
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
  /**
   * Tokens and dollars accrued so far in the running turn. Ignored unless `busy`;
   * absent degrades the line to spinner + elapsed — see `busyLine`.
   */
  turnTokens?: number | null;
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
    turnTokens,
    tick = 0,
    queued = [],
    notice,
    decorate,
    placeholder = "type to start · the agent writes one program per round",
  }: ChatProps,
) {
  // `height` is this component's TOTAL, not just the transcript's.
  //
  // The rows under the viewport — the scroll indicator, each queued message, the
  // busy line, a notice — used to be drawn IN ADDITION to a fixed-height body, so
  // Chat grew and shrank as they came and went and the composer under it slid up
  // and down the screen while you were typing into it. The input bar is the one
  // thing on screen that must never move; every other harness pins it. So the
  // extras are counted first and the transcript takes what is left.
  //
  // The activity strip is reserved WHETHER OR NOT a turn is running. It used to
  // appear with the spinner and vanish with it, so the whole transcript jumped one
  // row up at the start of every turn and one row down at the end — the settled
  // reply landed on a different line than the one you had just been reading.
  // The scroll indicator is reserved for the same reason, which also retires the
  // probe slice that used to resolve "the indicator needs a row iff there is one
  // to need it".
  // Shared with the click hit-test in the composition root — see `chatBodyHeight`.
  const body = chatBodyHeight(height, queued.length, Boolean(notice));
  const { start, rows, more, pct } = visibleSlice(lines, body, scrollOff);
  // Pad above, never below: the newest line stays where the eye already is.
  const pad = Math.max(0, body - rows.length);
  // Every row this component emits is blanked to the full width — see `padRow`.
  // A blank spacer of one space does not erase the row it lands on.
  const blank = padRow(" ", width);
  const busy2 = busyLine({
    activity,
    elapsedMs,
    tick,
    tokens: turnTokens,
  });
  return (
    <box flexDirection="column" width={width}>
      <box flexDirection="column" flexGrow={1}>
        {/*
          A FIXED SET OF SLOTS, keyed by screen row — never by transcript index.
          `key={l-${start + i}}` gave every row a fresh identity each time a
          streamed line shifted the window, so React unmounted and remounted the
          whole body on every token and the renderer re-placed ~20 renderables a
          frame. The viewport is `body` rows whose TEXT changes; the nodes do not.
        */}
        {Array.from({ length: body }, (_v, i) => {
          const line = lines.length === 0
            ? null
            : i >= pad
            ? rows[i - pad]
            : null;
          if (line) {
            const index = start + (i - pad);
            return (
              <MessageRow
                key={`row-${i}`}
                line={line}
                width={width}
                decorate={decorate && ((text) => decorate(text, line, index))}
              />
            );
          }
          // The empty-transcript hint sits on the last slot, where the first reply
          // will land.
          const text = lines.length === 0 && i === body - 1 ? padRow(placeholder, width) : blank;
          return (
            <text
              key={`row-${i}`}
              attributes={lines.length === 0 && i === body - 1 ? TextAttributes.DIM : undefined}
              wrapMode="none"
            >
              {text}
            </text>
          );
        })}
      </box>
      {more > 0
        ? (
          // Chrome, not content: the arrow and count carry the emphasis. The
          // percentage is the viewport TOP's position in the thread, so fully
          // scrolled up reads 0%.
          <text wrapMode="none">
            <span fg={UI.info}>↓ {more}</span>
            <span attributes={TextAttributes.DIM}>
              {padRow(
                ` more line${more === 1 ? "" : "s"} below · ${pct}%`,
                width - `↓ ${more}`.length,
              )}
            </span>
          </text>
        )
        : <text wrapMode="none">{blank}</text>}
      {queued.map((q, i) => (
        <text key={`q-${i}`} attributes={TextAttributes.DIM} wrapMode="none">
          {padRow(`⧖ queued: ${q}`, width)}
        </text>
      ))}
      {busy
        ? (
          // While a turn runs this REPLACES the bare activity blurb: it carries the
          // blurb's text when there is one, and says "working" when there is not,
          // so the running state is never a blank screen.
          <text wrapMode="none">
            <span fg={UI.accent}>{busy2.slice(0, 2)}</span>
            <span attributes={TextAttributes.DIM}>{padRow(busy2.slice(2), width - 2)}</span>
          </text>
        )
        : activity
        ? (
          <text wrapMode="none">
            <span fg={UI.accent}>{"⋯ "}</span>
            <span attributes={TextAttributes.DIM}>{padRow(activity, width - 2)}</span>
          </text>
        )
        // Reserved, not conditional — see `extras`.
        : <text wrapMode="none">{blank}</text>}
      {notice ? <text fg={UI.warn} wrapMode="none">{padRow(notice, width)}</text> : null}
    </box>
  );
}
