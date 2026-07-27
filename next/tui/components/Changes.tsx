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
 * merely avoided here.
 *
 * PURE CORE. `changeItems`, `fileStats` and `diffBody` are folds over the wire shape
 * with no React and no I/O; the component windows them and paints them. `+`/`-` colour
 * comes from the diff marker each line already carries, so nothing re-parses a hunk.
 *
 * Ported from `src/tui/components/DiffView.tsx`.
 */
import { Box, Text } from "ink";
import type { FileDiff } from "../../vcs/repodiff.ts";
import type { SessionChangeSet } from "../../server/changes.ts";
import { clip, windowAround } from "../format.ts";
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
}

export function Changes(
  { set, items, selected, scroll = 0, rows, focused = false, message }: ChangesProps,
) {
  if (!set) return <Text dimColor>loading changes…</Text>;
  if (!set.available) {
    return (
      <Box flexDirection="column">
        <Text color={palette.warn} wrap="wrap">{set.reason ?? "no change set here"}</Text>
        <Text dimColor wrap="wrap">
          the agent still works here — this checkout just produces nothing reviewable, and revert is
          unavailable
        </Text>
      </Box>
    );
  }
  const listRows = focused ? 0 : Math.max(1, Math.min(items.length, 6));
  const { start } = windowAround(selected, items.length, Math.max(1, listRows));
  const current = items[selected];
  const body = diffBody(current?.file);
  const bodyRows = Math.max(3, rows - (focused ? 5 : listRows + 7));
  const at = Math.max(0, Math.min(scroll, Math.max(0, body.length - bodyRows)));
  return (
    <Box flexDirection="column">
      {message ? <Text color={palette.warn} wrap="truncate">{message}</Text> : null}
      {items.length === 0
        ? <Text dimColor>no changes in this checkout yet</Text>
        : focused && current
        ? (
          <Text wrap="truncate">
            <Text color={statusColor(current.file.status)}>
              {STATUS_MARK[current.file.status]}
            </Text>{" "}
            <Text bold>{current.file.path}</Text>
          </Text>
        )
        : (
          <Box flexDirection="column">
            <Text>
              <Text bold>{items.length}</Text>
              <Text dimColor>{" "}file{items.length === 1 ? "" : "s"} changed</Text>
              {set.base ? <Text dimColor>{"  since "}{set.base.slice(0, 8)}</Text> : null}
            </Text>
            {items.slice(Math.max(0, start), Math.max(0, start) + listRows).map((item, i) => {
              const idx = Math.max(0, start) + i;
              const sel = idx === selected;
              return (
                <Text key={item.file.path} wrap="truncate">
                  <Text color={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</Text>
                  <Text color={sel ? undefined : statusColor(item.file.status)}>
                    {STATUS_MARK[item.file.status]}
                  </Text>{" "}
                  <Text bold={sel}>{clip(item.file.path, 48)}</Text>
                  <Text color={sel ? undefined : palette.accent}>{"  +"}{item.added}</Text>
                  <Text color={sel ? undefined : palette.error}>{" -"}{item.removed}</Text>
                </Text>
              );
            })}
          </Box>
        )}
      {body.length > 0
        ? (
          <Box flexDirection="column" marginTop={1}>
            {body.slice(at, at + bodyRows).map((line, i) => (
              <Text
                key={`${at + i}`}
                color={lineColor(line)}
                dimColor={!lineColor(line)}
                wrap="truncate-end"
              >
                {line || " "}
              </Text>
            ))}
            {body.length > bodyRows
              ? <Text dimColor>— {at + Math.min(bodyRows, body.length)}/{body.length} —</Text>
              : null}
          </Box>
        )
        : null}
      <Text dimColor wrap="truncate">
        {focused
          ? "← back · ↑↓ scroll the diff"
          : "↑↓ move · → focus one file · x revert this path · X revert everything"}
      </Text>
    </Box>
  );
}
