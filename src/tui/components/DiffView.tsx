import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import { SelRow } from "./SelRow.tsx";
import type { WireDiff, WireFileDiff } from "../api.ts";
import { windowAround } from "../format.ts";

export interface DiffEntry {
  source: WireDiff["source"];
  file: WireFileDiff;
  /** Carried from a subagent's unadopted group — `a` adopts instead of applying. */
  subagentId?: string;
  label?: string;
}

export function flattenDiffs(diffs: WireDiff[]): DiffEntry[] {
  return diffs.flatMap((d) =>
    d.files.map((file) => ({
      source: d.source,
      file,
      ...(d.subagentId ? { subagentId: d.subagentId, label: d.label } : {}),
    }))
  );
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
function statusColor(status: WireFileDiff["status"]): string {
  return status === "deleted" ? palette.error : status === "added" ? palette.accent : palette.warn;
}

export function DiffView(
  { entries, fileSel, scroll, rows, focused }: {
    entries: DiffEntry[];
    fileSel: number;
    scroll: number;
    rows: number;
    // Focused mode hides the file list so the selected file's hunks get the whole
    // panel (with many changed files the shared layout leaves them barely visible).
    focused: boolean;
  },
) {
  const fileRows = Math.max(2, Math.min(entries.length, 6));
  const { start: fileStart } = windowAround(fileSel, entries.length, fileRows);
  const sel = entries[fileSel];
  const lines = sel ? sel.file.hunks.flatMap((h) => [h.header, ...h.lines]) : [];
  const bodyRows = focused ? Math.max(4, rows - 4) : Math.max(4, rows - fileRows - 8);
  const at = Math.max(0, Math.min(scroll, lines.length - bodyRows));
  return (
    <Box flexDirection="column" marginTop={1}>
      {focused && sel
        ? (
          <Text bold>
            <Text color={statusColor(sel.file.status)}>{STATUS_MARK[sel.file.status]}</Text>{" "}
            {sel.file.path}
          </Text>
        )
        : (
          <Box flexDirection="column">
            <Text bold>
              changes{" "}
              <Text dimColor>
                {entries.length} file{entries.length === 1 ? "" : "s"}
              </Text>
            </Text>
            {entries.slice(fileStart, fileStart + fileRows).map((e, i) => {
              const { add, del } = stats(e.file);
              const rowSel = fileStart + i === fileSel;
              return (
                // Selected rows drop custom span colors: under inverse a colored fg
                // becomes a colored bg speck inside the light bar.
                <SelRow key={`${e.source}:${e.file.path}`} sel={rowSel}>
                  <Text color={rowSel ? undefined : statusColor(e.file.status)}>
                    {STATUS_MARK[e.file.status]}
                  </Text>{" "}
                  {e.file.path}
                  <Text color={rowSel ? undefined : palette.accent}>{"  "}+{add}</Text>
                  <Text color={rowSel ? undefined : palette.error}>{" "}-{del}</Text>
                  {e.label
                    ? <Text color={rowSel ? undefined : palette.warn}>{"  "}◆ {e.label}</Text>
                    : <Text dimColor>{"  "}{e.source}</Text>}
                </SelRow>
              );
            })}
            {entries.length === 0 && <Text dimColor>no changes on this session's branch</Text>}
          </Box>
        )}
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
      <Text dimColor>
        {focused
          ? "← back · j/k scroll"
          : sel?.subagentId
          ? "a adopt this subagent's changes · → focus · j/k scroll"
          : "a apply · A all · R revert · → focus · j/k scroll"}
      </Text>
    </Box>
  );
}
