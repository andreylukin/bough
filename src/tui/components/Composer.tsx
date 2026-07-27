/**
 * The input box, and the completion popup that sits on top of it.
 *
 * THE INVARIANT THIS HOLDS: **the cursor is exactly where the box says it is.**
 * The text is wrapped here, into fixed-width chunks, rather than by Ink — so the
 * character→row mapping is computed, not inferred from a layout pass. Each row is
 * then rendered `truncate`, which means Ink never reflows the block underneath the
 * cursor. A composer whose cursor drifts one row on a wrapped paste is a composer
 * people stop trusting with long input.
 *
 * SECOND INVARIANT — **the box never grows past its cap.** A large paste is
 * windowed to `maxRows` around the cursor with a counter row saying what is above
 * and below; the text itself is untouched. Growing to fit would push the
 * transcript off the screen exactly when the user is composing about it.
 *
 * THIRD, and the reason the completion state is all props: **this component is
 * presentational.** It does not fetch file names or skills, does not decide what
 * matches, and does not mutate a store — `activeTrigger` / `rankCompletions` in
 * `format.ts` do the pure part, and the container does the I/O. That split is what
 * makes the `@`/`/` behavior testable on strings with no server attached.
 *
 * ONE THING THE CANDIDATES MUST CARRY, stated here because this component cannot
 * enforce it: **`@` file candidates are expected to be gitignore-filtered at the
 * source.** The workspace listing that feeds them respects `.gitignore`, so
 * `node_modules` and build output never reach the popup. Filtering here would be
 * the wrong layer (it would need the workspace) and would silently disagree with
 * whatever the server actually searched.
 */
import { Box, Text } from "ink";
import type { Completion, Trigger } from "../format.ts";
import { UI } from "../format.ts";

export interface ComposerProps {
  input: string;
  cursor: number;
  /** A turn is running: Enter interjects into it rather than starting a new one. */
  busy: boolean;
  width: number;
  maxRows: number;
  /**
   * Dim autocomplete preview appended after the input. The caller guarantees the
   * cursor sits at end-of-input while one is shown; tab accepts it.
   */
  ghost?: string;
  /** The active `@`/`/` completion, or null. From `activeTrigger`. */
  trigger?: Trigger | null;
  /** Ranked rows for that trigger. From `rankCompletions`. */
  completions?: Completion[];
  /** Cursor within `completions`. -1 = browsing, nothing picked. */
  completionSel?: number;
  /** Matches hidden by the row cap, so the menu can say "↓ N more". */
  completionMore?: number;
}

export function Composer(
  {
    input,
    cursor,
    busy,
    width,
    maxRows,
    ghost = "",
    trigger = null,
    completions = [],
    completionSel = 0,
    completionMore = 0,
  }: ComposerProps,
) {
  // Wrap ourselves (fixed-width chunks) so the cursor→row mapping is exact.
  const innerW = Math.max(4, width - 4); // border + paddingX
  // An empty composer states the first action: without it the box reads as
  // decoration. Kept even when a ghost exists — a ghost only appears once you
  // have started typing, so the two never collide.
  const placeholder = input === "" ? "type a message · enter sends" : "";
  const ghostHint = ghost ? "  ⇥ tab" : "";
  const full = "› " + input + ghost + ghostHint;
  const ghostStart = 2 + input.length;
  const cur = cursor + 2;
  const rows: { start: number; text: string }[] = [];
  let off = 0;
  for (const line of full.split("\n")) {
    for (let i = 0;; i += innerW) {
      rows.push({ start: off + i, text: line.slice(i, i + innerW) });
      if (i + innerW >= line.length) break;
    }
    off += line.length + 1;
  }
  // The cursor's row: within [start, start+len), or sitting at the row's end when
  // nothing continues it there (end of a logical line, or end of text).
  const curRow = rows.findIndex((r, i) => {
    const end = r.start + r.text.length;
    return cur >= r.start &&
      (cur < end || (cur === end && (rows[i + 1]?.start ?? Infinity) > end));
  });
  const cap = Math.max(2, maxRows);
  const clipped = rows.length > cap;
  const shownCount = clipped ? cap - 1 : rows.length; // one row for the … counter
  const top = clipped
    ? Math.max(0, Math.min(curRow - (shownCount >> 1), rows.length - shownCount))
    : 0;
  const shown = rows.slice(top, top + shownCount);
  // A context hint under the box: a plain Enter mid-turn steers the running turn
  // rather than starting a new one, and saying so is the difference between
  // "queued" and "ignored" (spec §5).
  const hint = busy && input !== "" ? "enter interjects this turn" : "";
  return (
    <Box flexDirection="column">
      {trigger
        ? (
          <CompletionPopup
            kind={trigger.kind}
            items={completions}
            sel={completionSel}
            more={completionMore}
          />
        )
        : null}
      <Box
        flexDirection="column"
        borderStyle="round"
        // Accent while awaiting input: the composer is the focused element in
        // chat mode, and a hairline border made the first action invisible.
        borderColor={busy ? UI.warn : UI.accent}
        paddingX={1}
      >
        {shown.map((r, i) => {
          const hasCursor = top + i === curRow;
          const col = cur - r.start;
          const at = hasCursor ? r.text[col] : undefined;
          const prefix = r.start === 0 ? 2 : 0; // the accent "› " on the first row
          // Where this row crosses into ghost text — everything from there is dim.
          const gcol = Math.max(prefix, Math.min(ghostStart - r.start, r.text.length));
          return (
            <Text key={r.start} wrap="truncate">
              {prefix ? <Text color={UI.accent}>{"› "}</Text> : null}
              {hasCursor
                ? (
                  <>
                    {r.text.slice(prefix, col)}
                    <Text inverse>{at ?? " "}</Text>
                    {placeholder ? <Text dimColor>{placeholder}</Text> : null}
                    {at === undefined
                      ? ""
                      : <Text dimColor={col + 1 >= gcol}>{r.text.slice(col + 1)}</Text>}
                  </>
                )
                : r.text.length <= prefix
                ? " "
                : (
                  <>
                    {r.text.slice(prefix, gcol)}
                    {gcol < r.text.length ? <Text dimColor>{r.text.slice(gcol)}</Text> : null}
                  </>
                )}
            </Text>
          );
        })}
        {clipped
          ? (
            <Text dimColor>
              … {top} line{top === 1 ? "" : "s"} above · {rows.length - top - shownCount} below
            </Text>
          )
          : null}
        {hint ? <Text dimColor>{hint}</Text> : null}
      </Box>
    </Box>
  );
}

export interface CompletionPopupProps {
  kind: Trigger["kind"];
  items: Completion[];
  /** -1 = browsing; Enter then keeps the typed text rather than a listed row. */
  sel: number;
  more: number;
}

/**
 * The `@`/`/` menu. A filter that matches nothing still shows the box, saying so:
 * silently hiding it reads as "the picker is broken" rather than "no such file".
 */
export function CompletionPopup({ kind, items, sel, more }: CompletionPopupProps) {
  return (
    <Box flexDirection="column" borderStyle="round" borderColor={UI.muted} paddingX={1}>
      {items.length === 0
        ? <Text dimColor>{kind === "file" ? "no matching files" : "no matching skills"}</Text>
        : items.map((it, i) => {
          const selected = i === sel;
          // File rows dim the directory prefix so basenames stand out — skipped
          // on the selected row, where dim under the inverse bar goes illegible.
          const dimTo = kind === "file" && !selected ? it.label.lastIndexOf("/") + 1 : 0;
          return (
            <Text key={it.label} inverse={selected} wrap="truncate">
              <PopupLabel label={it.label} hl={it.hl} dimTo={dimTo} />
              {it.detail ? <Text dimColor={!selected}>{"  "}{it.detail}</Text> : null}
            </Text>
          );
        })}
      {more > 0
        ? (
          // Keeps the row cap honest: without this a first-run user reads the
          // menu as the whole catalogue and never types to narrow it.
          <Text>
            <Text color={UI.info}>↓ {more}</Text>
            <Text dimColor>{" "}more — keep typing to narrow</Text>
          </Text>
        )
        : null}
      <Text dimColor>
        {kind === "file"
          ? "files & dirs — ↑↓ select · tab inserts · esc closes"
          : "skills — ↑↓ select · tab inserts · esc closes"}
      </Text>
    </Box>
  );
}

/** A label with the fuzzy-matched characters emphasized. */
function PopupLabel({ label, hl, dimTo }: { label: string; hl?: number[]; dimTo: number }) {
  const marks = new Set(hl ?? []);
  if (marks.size === 0) {
    return dimTo > 0
      ? (
        <Text>
          <Text dimColor>{label.slice(0, dimTo)}</Text>
          {label.slice(dimTo)}
        </Text>
      )
      : <Text>{label}</Text>;
  }
  return (
    <Text>
      {[...label].map((ch, i) => (
        marks.has(i)
          ? <Text key={i} bold color={UI.accent}>{ch}</Text>
          : <Text key={i} dimColor={i < dimTo}>{ch}</Text>
      ))}
    </Text>
  );
}
