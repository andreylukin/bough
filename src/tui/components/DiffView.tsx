import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { WireDiff, WireFileDiff } from "../api.ts";

export interface DiffEntry {
  source: WireDiff["source"];
  file: WireFileDiff;
}

export function flattenDiffs(diffs: WireDiff[]): DiffEntry[] {
  return diffs.flatMap((d) => d.files.map((file) => ({ source: d.source, file })));
}

function stats(f: WireFileDiff): { add: number; del: number } {
  let add = 0, del = 0;
  for (const h of f.hunks) {
    for (const l of h.lines) {
      if (l.startsWith("+")) add++;
      else if (l.startsWith("-")) del++;
    }
  }
  return { add, del };
}

const STATUS_MARK = { added: "A", modified: "M", deleted: "D" } as const;

function DiffLine({ line }: { line: string }) {
  const color = line.startsWith("+")
    ? palette.accent
    : line.startsWith("-")
    ? palette.error
    : undefined;
  return <Text color={color} dimColor={!color} wrap="truncate-end">{line || " "}</Text>;
}

// Changes review: file list on top, the selected file's hunks in a scrollable
// window below. Long diffs scroll with j/k; apply/revert act via the store.
// Content-only — the unified panel container owns the border + tab bar.
export function DiffView(
  { entries, fileSel, scroll, rows }: {
    entries: DiffEntry[];
    fileSel: number;
    scroll: number;
    rows: number;
  },
) {
  const fileRows = Math.max(2, Math.min(entries.length, 6));
  const fileStart = Math.max(
    0,
    Math.min(fileSel - Math.floor(fileRows / 2), entries.length - fileRows),
  );
  const sel = entries[fileSel];
  const lines = sel ? sel.file.hunks.flatMap((h) => [h.header, ...h.lines]) : [];
  const bodyRows = Math.max(4, rows - fileRows - 8);
  const at = Math.max(0, Math.min(scroll, lines.length - bodyRows));
  return (
    <Box flexDirection="column" marginTop={1}>
      <Text bold>
        changes{" "}
        <Text dimColor>
          {entries.length} file{entries.length === 1 ? "" : "s"}
        </Text>
      </Text>
      {entries.slice(fileStart, fileStart + fileRows).map((e, i) => {
        const { add, del } = stats(e.file);
        return (
          <Text
            key={`${e.source}:${e.file.path}`}
            inverse={fileStart + i === fileSel}
            wrap="truncate"
          >
            <Text
              color={e.file.status === "deleted"
                ? palette.error
                : e.file.status === "added"
                ? palette.accent
                : palette.warn}
            >
              {STATUS_MARK[e.file.status]}
            </Text>{" "}
            {e.file.path}
            <Text color={palette.accent}>{"  "}+{add}</Text>
            <Text color={palette.error}>{" "}-{del}</Text>
            <Text dimColor>{"  "}{e.source}</Text>
          </Text>
        );
      })}
      {entries.length === 0 && <Text dimColor>no pending changes</Text>}
      {sel && (
        <Box flexDirection="column" marginTop={1}>
          {lines.slice(at, at + bodyRows).map((l, i) => <DiffLine key={at + i} line={l} />)}
          {lines.length > bodyRows && (
            <Text dimColor>
              — {at + Math.min(bodyRows, lines.length)}/{lines.length} —
            </Text>
          )}
        </Box>
      )}
    </Box>
  );
}
