import { Box, Text } from "ink";
import { relTime } from "../format.ts";
import type { TuiSession } from "../store.ts";

export interface TreeRow {
  s: TuiSession;
  depth: number;
}

const VISIBLE_KINDS = new Set(["root", "fork", "compaction"]);

// Flatten the session tree for the picker: roots newest-first, children (forks/
// compactions) indented under their parent oldest-first. A child whose parent
// isn't visible (e.g. archived) surfaces as a root.
export function flattenTree(all: TuiSession[]): TreeRow[] {
  const visible = all.filter((s) => VISIBLE_KINDS.has(s.kind));
  const ids = new Set(visible.map((s) => s.id));
  const byParent = new Map<string | null, TuiSession[]>();
  for (const s of visible) {
    const p = s.parentId && ids.has(s.parentId) ? s.parentId : null;
    byParent.set(p, [...(byParent.get(p) ?? []), s]);
  }
  const out: TreeRow[] = [];
  const walk = (s: TuiSession, depth: number) => {
    out.push({ s, depth });
    for (const c of (byParent.get(s.id) ?? []).sort((a, b) => a.createdAt - b.createdAt)) {
      walk(c, depth + 1);
    }
  };
  for (const r of (byParent.get(null) ?? []).sort((a, b) => b.createdAt - a.createdAt)) walk(r, 0);
  return out;
}

const KIND_MARK: Record<string, string> = { fork: "⑂ ", compaction: "≣ " };

export function SessionPicker(
  { rowsList, selected, filter, filterActive, rows }: {
    rowsList: TreeRow[];
    selected: number;
    filter: string;
    /** `/` puts the picker in filter-entry mode; otherwise keys navigate. */
    filterActive: boolean;
    rows: number;
  },
) {
  const max = Math.max(3, rows - 7);
  const start = Math.max(0, Math.min(selected - Math.floor(max / 2), rowsList.length - max));
  const win = rowsList.slice(start, start + max);
  return (
    <Box flexDirection="column" borderStyle="round" borderColor="gray" paddingX={1}>
      <Text bold>sessions</Text>
      {filterActive
        ? (
          <Text>
            / {filter}
            <Text inverse>{" "}</Text>
          </Text>
        )
        : filter
        ? <Text dimColor>/ {filter}</Text>
        : null}
      {win.map(({ s, depth }, i) => {
        const sel = start + i === selected;
        const dot = s.busy ? "⋯" : s.unseen ? "●" : " ";
        return (
          <Text key={s.id} inverse={sel} wrap="truncate">
            <Text color={s.busy ? "yellow" : "green"}>{dot}</Text> {"  ".repeat(depth)}
            {KIND_MARK[s.kind] ?? ""}
            {s.title || "(untitled)"}
            <Text dimColor>{"  "}{relTime(s.createdAt)} ago</Text>
          </Text>
        );
      })}
      {rowsList.length === 0 && <Text dimColor>no sessions — ^t creates one</Text>}
    </Box>
  );
}
