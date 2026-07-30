/**
 * The changes tab: what this session did to the checkout, and the one way to undo it.
 *
 * THE INVARIANT THIS HOLDS: **"not a repository" is an answer, never an empty diff.**
 * Spec §13 makes the distinction load-bearing — "this workspace is not a repository"
 * and "you changed nothing" are different facts, and a rail that rendered both as an
 * empty list would be lying about one of them. `SessionChangeSet` carries
 * `available: false` with a `reason` sentence written server-side (`server/changes.ts`),
 * and this file's first job is to show that sentence rather than fall through to the
 * file list. The old `DiffView.tsx` had no such state and no such branch.
 *
 * SECOND: **revert is the only mutation, and it is per path.** There is no apply — the
 * agent edits the user's checkout in place, so the work is already where an apply
 * would have put it, and delivery is the reviewer's own `git commit` (spec §13, §17).
 * The old view's `enter apply · A all` hints are dropped for exactly that reason. What
 * a keypress here does is send the SELECTED path to `POST …/changes/revert`; the
 * server intersects it with the change set, so a stale path is refused there and not
 * merely avoided here. And it takes TWO keypresses: `x` arms a revert and prints what
 * it will destroy, ⏎ performs it — a destructive verb states its scope and waits,
 * because a key that deletes a file on the first press is a key the cursor lands on.
 *
 * PURE CORE. `changeItems`, `fileStats` and `diffBody` are folds over the wire shape
 * with no React and no I/O; the component windows them and paints them. `+`/`-` colour
 * comes from the diff marker each line already carries, so nothing re-parses a hunk.
 *
 * Ported from `src/tui/components/DiffView.tsx`.
 */
import { TextAttributes } from "@opentui/core";
import type { FileDiff } from "../../vcs/repodiff.ts";
import type { SessionChangeSet } from "../../server/changes.ts";
import { clip, legendLine, windowAround } from "../format.ts";
import { palette } from "../theme.ts";

// ---------------------------------------------------------------------------
// Pure core
// ---------------------------------------------------------------------------

export interface ChangeItem {
  file: FileDiff;
  added: number;
  removed: number;
}

/** Added/removed line counts. `+++`/`---` never appear — the server sends hunks only. */
export function fileStats(f: FileDiff): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const hunk of f.hunks) {
    for (const line of hunk.lines) {
      if (line.startsWith("+")) added++;
      else if (line.startsWith("-")) removed++;
    }
  }
  return { added, removed };
}

/** The file list, in the server's order — git's, which is already path-sorted. */
export function changeItems(set: SessionChangeSet | null): ChangeItem[] {
  if (!set?.available) return [];
  return set.files.map((file) => ({ file, ...fileStats(file) }));
}

/** One file's hunks flattened to display lines, headers included. */
export function diffBody(f: FileDiff | undefined): string[] {
  if (!f) return [];
  if (f.hunks.length === 0) {
    // Binary or unreadable content yields no hunks — say which, do not show blank.
    return [`(no textual diff — ${f.status})`];
  }
  return f.hunks.flatMap((h) => [h.header, ...h.lines]);
}

const STATUS_MARK: Record<FileDiff["status"], string> = {
  added: "A",
  modified: "M",
  deleted: "D",
};

/**
 * A revert that has been asked for and not yet done.
 *
 * Two scopes and no third: one path, or the whole change set. `x` arms the first and
 * pressing it again WIDENS to the second — the escalation is a second deliberate
 * keypress against a printed blast radius, never an inference from the first.
 */
export type PendingRevert = { scope: "file"; item: ChangeItem } | { scope: "all" };

/**
 * What reverting THIS row will do, in words, before it is done.
 *
 * The consent rule: a destructive verb names its own blast radius — the file, what
 * happens to it, and what is left alone. "revert" is not self-explanatory here: on a
 * file this session ADDED it means delete, and a dialog that does not say so is asking
 * for a yes to a question the user did not read.
 */
export function revertScope(item: ChangeItem, total: number): string {
  const what = item.file.status === "added"
    ? "added by this session — reverting DELETES it"
    : item.file.status === "deleted"
    ? "deleted by this session — reverting RESTORES it"
    : `modified by this session — reverting DISCARDS +${item.added} -${item.removed}`;
  const rest = total - 1;
  if (rest <= 0) return what;
  return `${what}; the other ${rest} file${rest === 1 ? " is" : "s are"} untouched`;
}

function statusColor(status: FileDiff["status"]): string {
  return status === "deleted" ? palette.error : status === "added" ? palette.accent : palette.warn;
}

function lineColor(line: string): string | undefined {
  if (line.startsWith("@@")) return palette.info;
  if (line.startsWith("+")) return palette.accent;
  if (line.startsWith("-")) return palette.error;
  return undefined;
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

export interface ChangesProps {
  /** Columns available, so the legend degrades instead of being cut mid-word. */
  cols?: number;
  /** `null` while the fetch is in flight — distinct from an unavailable change set. */
  set: SessionChangeSet | null;
  items: ChangeItem[];
  selected: number;
  /** Lines scrolled into the hunk body of the selected file. */
  scroll?: number;
  rows: number;
  /**
   * Hide the file list and give the whole tab to one file's hunks. With many changed
   * files the shared layout leaves the diff itself barely visible.
   */
  focused?: boolean;
  /** Result of the last revert, or why one was refused. */
  message?: string | null;
  /**
   * The revert waiting for a yes. `x` arms it; ⏎ performs it and esc cancels
   * (`PanelHost.tsx`). Revert deletes files, so the keypress and the deletion are
   * two different events with the scope printed between them — the same grammar the
   * ask card uses, kept inside the tab because the panel owns the keyboard while it
   * is open.
   */
  pending?: PendingRevert | null;
  /**
   * The line printed under an unavailable change set. Defaults to the non-git
   * sentence, which is the case spec §13 names. `null` suppresses it — the "no
   * conversation is open" case has no checkout at all, and telling the user that
   * "the agent still works here" about a workspace that does not exist is a claim
   * this component cannot make.
   */
  hint?: string | null;
}

/** Spec §13's non-git case: the agent works, it just produces nothing reviewable. */
export const NOT_A_REPO_HINT =
  "the agent still works here — its edits just aren't reviewable, and revert is unavailable";

export function Changes(
  {
    set,
    items,
    selected,
    scroll = 0,
    rows,
    focused = false,
    message,
    pending = null,
    hint = NOT_A_REPO_HINT,
    cols,
  }: ChangesProps,
) {
  if (!set) return <text attributes={TextAttributes.DIM}>loading changes…</text>;
  if (!set.available) {
    return (
      <box flexDirection="column">
        <text fg={palette.warn} wrapMode="word">{set.reason ?? "no change set here"}</text>
        {hint ? <text attributes={TextAttributes.DIM} wrapMode="word">{hint}</text> : null}
        {/* Even with nothing to review the tab ends in a legend, because the legend
            is where a reader looks and "there is no way out of here" is never true. */}
        <text attributes={TextAttributes.DIM} wrapMode="none">esc back · ^t close</text>
      </box>
    );
  }
  // THE ROW BUDGET, COUNTED RATHER THAN GUESSED. The old arithmetic subtracted a
  // constant 5 or 7 and then floored the diff at three rows, so at twelve terminal
  // rows this tab asked for more rows than it had and OpenTUI answered by shrinking
  // them onto each other — see `Panel.tsx`. Everything below is counted:
  //   message?  ·  the file list (a header + up to six rows)  ·  the diff (a blank
  //   separator + body + an optional `— n/m —`)  ·  the legend, or the 3-row confirm.
  const msgRows = message ? 1 : 0;
  // The dialog takes rows from the DIFF rather than from the bottom of the screen:
  // a confirm the panel scrolled off is a confirm nobody read.
  const footRows = pending ? 3 : 1;
  const listRows = focused
    ? 0
    : Math.min(items.length, Math.max(1, Math.min(6, rows - msgRows - footRows - 2)));
  const { start } = windowAround(selected, items.length, Math.max(1, listRows));
  const current = items[selected];
  const body = diffBody(current?.file);
  const headRows = items.length === 0 ? 1 : focused ? 1 : 1 + listRows;
  // What is left for the diff, after its own blank separator row.
  const room = Math.max(0, rows - msgRows - headRows - footRows - 1);
  const bodyRows = body.length > room ? Math.max(0, room - 1) : room;
  const at = Math.max(0, Math.min(scroll, Math.max(0, body.length - bodyRows)));
  return (
    <box flexDirection="column">
      {message ? <text fg={palette.warn} wrapMode="none">{message}</text> : null}
      {items.length === 0
        ? <text attributes={TextAttributes.DIM}>no changes in this checkout yet</text>
        : focused && current
        ? (
          <text wrapMode="none">
            <span fg={statusColor(current.file.status)}>
              {STATUS_MARK[current.file.status]}
            </span>{" "}
            <b>{current.file.path}</b>
          </text>
        )
        : (
          <box flexDirection="column">
            <text>
              <b>{items.length}</b>
              <span attributes={TextAttributes.DIM}>
                {" "}file{items.length === 1 ? "" : "s"} changed
              </span>
              {set.base
                ? <span attributes={TextAttributes.DIM}>{"  since "}{set.base.slice(0, 8)}</span>
                : null}
            </text>
            {items.slice(Math.max(0, start), Math.max(0, start) + listRows).map((item, i) => {
              const idx = Math.max(0, start) + i;
              const sel = idx === selected;
              return (
                <text key={item.file.path} wrapMode="none">
                  <span fg={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</span>
                  <span fg={sel ? undefined : statusColor(item.file.status)}>
                    {STATUS_MARK[item.file.status]}
                  </span>{" "}
                  <span attributes={sel ? TextAttributes.BOLD : TextAttributes.NONE}>
                    {clip(item.file.path, 48)}
                  </span>
                  <span fg={sel ? undefined : palette.accent}>{"  +"}{item.added}</span>
                  <span fg={sel ? undefined : palette.error}>{" -"}{item.removed}</span>
                </text>
              );
            })}
          </box>
        )}
      {body.length > 0 && bodyRows > 0
        ? (
          <box flexDirection="column" marginTop={1}>
            {body.slice(at, at + bodyRows).map((line, i) => (
              <text
                key={`${at + i}`}
                fg={lineColor(line)}
                attributes={lineColor(line) ? TextAttributes.NONE : TextAttributes.DIM}
                wrapMode="none"
              >
                {line || " "}
              </text>
            ))}
            {body.length > bodyRows
              ? (
                <text attributes={TextAttributes.DIM}>
                  — {at + Math.min(bodyRows, body.length)}/{body.length} —
                </text>
              )
              : null}
          </box>
        )
        : null}
      {/*
        The legend is the tab's LAST row, and it names the keys the keymap now binds.
        `x` was reaching this tab only because it was `wf.stop` and the dispatcher
        re-routed it by hand; it is `changes.revert` with `tab: ["changes"]` now, and
        `X` — the whole change set — could not reach this tab at all, so the legend
        could not honestly name it. It can.
      */}
      {pending
        ? <RevertConfirm pending={pending} items={items} base={set.base} />
        : (
          <text attributes={TextAttributes.DIM} wrapMode="none">
            {focused
              ? legendLine([
                "← back",
                "↑↓ scroll the diff",
                "x revert this path",
                "X revert everything",
              ], cols)
              : legendLine([
                "↑↓ move",
                "→ focus one file",
                "x revert this path",
                "X revert all",
                "esc back",
              ], cols)}
          </text>
        )}
    </box>
  );
}

/**
 * The yes/no, printed where the legend was.
 *
 * It replaces the legend rather than joining it, because the keys that mean something
 * while a revert is armed are not the keys that mean something otherwise — a footer
 * listing both would be listing the ones that are inert.
 */
function RevertConfirm(
  { pending, items, base }: {
    pending: PendingRevert;
    items: ChangeItem[];
    base: string | null;
  },
) {
  if (pending.scope === "all") {
    const added = items.reduce((n, i) => n + i.added, 0);
    const removed = items.reduce((n, i) => n + i.removed, 0);
    return (
      <box flexDirection="column">
        <text fg={palette.error} wrapMode="none">
          <b>revert all {items.length} files (+{added} -{removed})?</b>
        </text>
        <text attributes={TextAttributes.DIM} wrapMode="none">
          everything this session touched goes back{base ? ` to ${base.slice(0, 8)}` : ""}, and
          files it created are deleted
        </text>
        <text wrapMode="none">
          <span fg={palette.error}>⏎ revert everything</span>
          <span attributes={TextAttributes.DIM}>{" · esc cancel"}</span>
        </text>
      </box>
    );
  }
  return (
    <box flexDirection="column">
      <text fg={palette.warn} wrapMode="none">
        <b>revert {pending.item.file.path}?</b>
      </text>
      <text attributes={TextAttributes.DIM} wrapMode="none">
        {revertScope(pending.item, items.length)}
      </text>
      <text wrapMode="none">
        <span fg={palette.warn}>⏎ revert it</span>
        <span attributes={TextAttributes.DIM}>
          {/* `X`, not a second `x`. The escalation used to ride the second `x` — the
              same gesture the rail teaches as "arm, then confirm" — so the reflex
              landed on "revert all N files" one ⏎ from wiping the session's work
              (`PanelHost.tsx`). The capital is a separate key and a separate decision. */}
          {items.length > 1 ? `  ·  X all ${items.length} files` : ""}
          {"  ·  esc cancel"}
        </span>
      </text>
    </box>
  );
}
