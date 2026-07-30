/**
 * The input box, and the completion popup that sits on top of it.
 *
 * THE INVARIANT THIS HOLDS: **the cursor is exactly where the box says it is.**
 * The text is wrapped here, into fixed-width chunks, rather than by the renderer —
 * so the character→row mapping is computed, not inferred from a layout pass. Each
 * row is then rendered `wrapMode="none"`, which means the renderer never reflows the
 * block underneath the cursor. A composer whose cursor drifts one row on a wrapped
 * paste is a composer people stop trusting with long input.
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
import { TextAttributes } from "@opentui/core";
import type { Completion, Trigger } from "../format.ts";
import { UI } from "../format.ts";

/**
 * The block caret's foreground, against an accent background.
 *
 * NOT `TextAttributes.INVERSE`, which is what this was and which renders the caret
 * INVISIBLE: OpenTUI double-signals reverse video — it writes an explicit white
 * background AND leaves SGR 7 set — so the terminal inverts an already-inverted
 * pair back to white-on-white. A cell dump of the caret returned `fg #ffffff bg
 * #ffffff inverse=true`, i.e. a composer with no visible cursor at all. An explicit
 * pair states the colours once and cannot be flipped twice.
 */
const CARET_FG = "black";

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
  /**
   * The surface that has the keyboard INSTEAD of this one, e.g. `"the tree"`.
   * Null (the default) means the composer is focused.
   *
   * THE LIE THIS ENDS: the composer is pinned under the panel, the rail and the job
   * view, and it painted an accent border, a block caret and "type a message · enter
   * sends" in every one of them — while `App` drops typed characters unless the mode
   * is `chat`. Open the tree, type a correction, press Enter: 27 characters and the
   * Return vanished, and Enter opened whatever row the tree cursor was on. Nothing on
   * screen had said the box was inert. A component that looks focused must be focused.
   */
  keyboardOwner?: string | null;
}

/**
 * Rows the box will draw, so the container can SIZE the region above it instead
 * of guessing.
 *
 * `App` used to reserve `min(8, max(3, rows/4))` rows for this component and then
 * subtract a further constant 4 "for chrome". At 34 rows that reserved 12 for a
 * box that draws 3, so a quarter of the terminal below the status line was never
 * painted at all and the input bar floated six rows off the bottom. A guess is
 * the wrong shape here: the height depends on the draft's wrapping and on whether
 * the hint row is showing, both of which only this file knows. Mirrors the render
 * below — the two must be edited together.
 */
export function composerHeight(
  { input, ghost = "", busy, width, maxRows }: Pick<
    ComposerProps,
    "input" | "busy" | "width" | "maxRows"
  > & { ghost?: string },
): number {
  const innerW = Math.max(4, width - 4);
  const full = "› " + input + ghost + (ghost ? "  ⇥ tab" : "");
  let n = 0;
  for (const line of full.split("\n")) n += Math.max(1, Math.ceil(line.length / innerW));
  const cap = Math.max(2, maxRows);
  const clipped = n > cap;
  const hint = (busy && input !== "") || input.startsWith("!") ? 1 : 0;
  return 2 /* border */ + (clipped ? cap - 1 : n) + (clipped ? 1 : 0) + hint;
}

/** Rows `CompletionPopup` will draw, for the same reason as `composerHeight`. */
export function completionPopupHeight(items: number, more: number): number {
  return 2 /* border */ + Math.max(1, items) + (more > 0 ? 1 : 0) + 1 /* legend */;
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
    keyboardOwner = null,
  }: ComposerProps,
) {
  // Wrap ourselves (fixed-width chunks) so the cursor→row mapping is exact.
  const innerW = Math.max(4, width - 4); // border + paddingX
  // An empty composer states the first action: without it the box reads as
  // decoration. Kept even when a ghost exists — a ghost only appears once you
  // have started typing, so the two never collide.
  // When another surface has the keyboard the placeholder names it instead, because
  // the empty box is exactly where "type a message" is read as an invitation. A
  // draft that IS there stays visible and untouched — it is still your draft, it
  // just is not taking keys this second.
  const placeholder = keyboardOwner
    ? (input === "" ? `${keyboardOwner} has the keyboard · esc returns here` : "")
    : input === ""
    ? "type a message · enter sends"
    : "";
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
  //
  // The second says where a `!` line is about to go. It used to deny the sigil
  // outright ("! is not a shell — this goes to the model"), which was honest about a
  // real gap: `!echo hi` reached the frontier model as a prompt, titled the session
  // "Echo Command Test", and billed a round trip. The sigil is honoured now, so the
  // hint says what it does instead — a shell in the workspace, not a message, which
  // is the one thing the user needs to know before pressing Enter.
  const hint = busy && input !== ""
    ? "enter interjects this turn"
    : input.startsWith("!")
    ? "runs in your shell · not a message · output lands in the rail"
    : "";
  return (
    <box flexDirection="column">
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
      <box
        flexDirection="column"
        borderStyle="rounded"
        // Accent while awaiting input: the composer is the focused element in
        // chat mode, and a hairline border made the first action invisible. A
        // hairline is right for the opposite case — another surface holds the
        // keyboard and this box is not where the next keystroke lands.
        borderColor={keyboardOwner ? UI.muted : busy ? UI.warn : UI.accent}
        paddingX={1}
      >
        {shown.map((r, i) => {
          // No caret when the keyboard is elsewhere: a block cursor is the single
          // strongest claim a terminal UI can make about where typing goes.
          const hasCursor = !keyboardOwner && top + i === curRow;
          const col = cur - r.start;
          const at = hasCursor ? r.text[col] : undefined;
          const prefix = r.start === 0 ? 2 : 0; // the accent "› " on the first row
          // Where this row crosses into ghost text — everything from there is dim.
          const gcol = Math.max(prefix, Math.min(ghostStart - r.start, r.text.length));
          return (
            <text key={r.start} wrapMode="none">
              {prefix ? <span fg={UI.accent}>{"› "}</span> : null}
              {hasCursor
                ? (
                  <>
                    {r.text.slice(prefix, col)}
                    <span fg={CARET_FG} bg={UI.accent}>{at ?? " "}</span>
                    {placeholder
                      ? <span attributes={TextAttributes.DIM}>{placeholder}</span>
                      : null}
                    {at === undefined
                      ? ""
                      : (
                        <span
                          attributes={col + 1 >= gcol ? TextAttributes.DIM : TextAttributes.NONE}
                        >
                          {r.text.slice(col + 1)}
                        </span>
                      )}
                  </>
                )
                // An empty first row with no caret: the placeholder lives here
                // instead. Unfocused, the caret branch above never runs, and
                // without this the box that has just lost the keyboard is the one
                // that says nothing about it.
                : r.text.length <= prefix
                ? (placeholder
                  ? <span attributes={TextAttributes.DIM}>{placeholder}</span>
                  : " ")
                : (
                  <>
                    {r.text.slice(prefix, gcol)}
                    {gcol < r.text.length
                      ? <span attributes={TextAttributes.DIM}>{r.text.slice(gcol)}</span>
                      : null}
                  </>
                )}
            </text>
          );
        })}
        {clipped
          ? (
            <text attributes={TextAttributes.DIM}>
              … {top} line{top === 1 ? "" : "s"} above · {rows.length - top - shownCount} below
            </text>
          )
          : null}
        {hint ? <text attributes={TextAttributes.DIM}>{hint}</text> : null}
      </box>
    </box>
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
    <box flexDirection="column" borderStyle="rounded" borderColor={UI.muted} paddingX={1}>
      {items.length === 0
        ? (
          <text attributes={TextAttributes.DIM}>
            {kind === "file" ? "no matching files" : "no matching commands or skills"}
          </text>
        )
        : items.map((it, i) => {
          const selected = i === sel;
          // File rows dim the directory prefix so basenames stand out.
          const dimTo = kind === "file" ? it.label.lastIndexOf("/") + 1 : 0;
          return (
            <text key={it.label} wrapMode="none">
              {/* A `❯` and an accent, not a reverse-video bar: reverse renders
                  white-on-white here (see CARET_FG), so the row Enter was about to
                  act on was marked with nothing at all. This is also the cursor
                  Sessions, Changes, ModelPicker and Theme already use, which is the
                  point — one cursor glyph across every list in the TUI. */}
              <span fg={UI.accent}>{selected ? "❯ " : "  "}</span>
              <PopupLabel label={it.label} hl={it.hl} dimTo={dimTo} />
              {it.detail
                ? <span attributes={TextAttributes.DIM}>{"  "}{it.detail}</span>
                : null}
            </text>
          );
        })}
      {more > 0
        ? (
          // Keeps the row cap honest: without this a first-run user reads the
          // menu as the whole catalogue and never types to narrow it.
          <text>
            <span fg={UI.info}>↓ {more}</span>
            <span attributes={TextAttributes.DIM}>{" "}more — keep typing to narrow</span>
          </text>
        )
        : null}
      <text attributes={TextAttributes.DIM}>
        {/* ⏎ is named FIRST because it is now the commit key here too: every
            bordered list in this TUI affirms on Enter, and the pickers were the one
            widget where it discarded the highlighted row and sent the raw draft. */}
        {/* "inserts" is only half true on the `/` list now: a built-in command row
            RUNS, a skill row inserts. The legend says both rather than promising
            the one behaviour that would be wrong for whichever row is highlighted. */}
        {kind === "file"
          ? "files & dirs — ↑↓ select · ⏎ or ⇥ inserts · esc closes"
          : "commands & skills — ↑↓ select · ⏎ runs or inserts · esc closes"}
      </text>
    </box>
  );
}

/** A label with the fuzzy-matched characters emphasized. */
function PopupLabel({ label, hl, dimTo }: { label: string; hl?: number[]; dimTo: number }) {
  const marks = new Set(hl ?? []);
  if (marks.size === 0) {
    return dimTo > 0
      ? (
        <span>
          <span attributes={TextAttributes.DIM}>{label.slice(0, dimTo)}</span>
          {label.slice(dimTo)}
        </span>
      )
      : <span>{label}</span>;
  }
  return (
    <span>
      {[...label].map((ch, i) => (
        marks.has(i)
          ? <span key={i} attributes={TextAttributes.BOLD} fg={UI.accent}>{ch}</span>
          : (
            <span key={i} attributes={i < dimTo ? TextAttributes.DIM : TextAttributes.NONE}>
              {ch}
            </span>
          )
      ))}
    </span>
  );
}
