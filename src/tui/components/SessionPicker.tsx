import { Box, Text } from "ink";
import { relTime } from "../format.ts";
import type { TuiSession } from "../store.ts";

export interface TreeRow {
  s: TuiSession;
  depth: number;
  /** Box-drawing connector prefix (│ ├ └) drawn to this row's glyph. */
  prefix: string;
}

const VISIBLE_KINDS = new Set(["root", "fork", "compaction", "subagent"]);

// A session's lineage parent is what it BRANCHED FROM: originId (forks, compactions,
// subagents, extracted roots all record it). parentId is a jj sibling artifact and
// only used as a fallback. A root has neither → it's a trunk.
function parentOf(s: TuiSession): string | null {
  return s.originId ?? s.parentId ?? null;
}

// Flatten the lineage forest for the picker with tree-drawing connectors. Trunks
// (roots) newest-first; branches (forks, compactions, subagents) oldest-first under
// what they branched from, so a session's history reads top-to-bottom.
export function flattenTree(all: TuiSession[]): TreeRow[] {
  const visible = all.filter((s) => VISIBLE_KINDS.has(s.kind));
  const ids = new Set(visible.map((s) => s.id));
  const byParent = new Map<string | null, TuiSession[]>();
  for (const s of visible) {
    const parent = parentOf(s);
    const p = parent && ids.has(parent) ? parent : null;
    byParent.set(p, [...(byParent.get(p) ?? []), s]);
  }
  const out: TreeRow[] = [];
  const walk = (s: TuiSession, prefix: string, isLast: boolean, depth: number) => {
    const connector = depth === 0 ? "" : isLast ? "└─" : "├─";
    out.push({ s, depth, prefix: prefix + connector });
    const kids = (byParent.get(s.id) ?? []).sort((a, b) => a.createdAt - b.createdAt);
    const childPrefix = depth === 0 ? "" : prefix + (isLast ? "  " : "│ ");
    kids.forEach((c, j) => walk(c, childPrefix, j === kids.length - 1, depth + 1));
  };
  const roots = (byParent.get(null) ?? []).sort((a, b) => b.createdAt - a.createdAt);
  roots.forEach((r) => walk(r, "", true, 0));
  return out;
}

// Per-kind glyph + how to clean the auto-generated title prefix.
const KIND: Record<string, { glyph: string; color?: string; strip?: RegExp }> = {
  root: { glyph: "●", color: "green" },
  fork: { glyph: "⑂", strip: /^fork · / },
  compaction: { glyph: "≣", strip: /^compacted · / },
  subagent: { glyph: "◆", color: "green", strip: /^subagent · / },
};

export function SessionPicker(
  { rowsList, selected, filter, filterActive, rows, currentId, showDeprecated }: {
    rowsList: TreeRow[];
    selected: number;
    filter: string;
    /** `/` puts the picker in filter-entry mode; otherwise keys navigate. */
    filterActive: boolean;
    rows: number;
    /** The open session — marked "you are here" in the tree. */
    currentId: string | null;
    /** Whether deprecated branches are currently revealed. */
    showDeprecated: boolean;
  },
) {
  const max = Math.max(3, rows - 9);
  const start = Math.max(0, Math.min(selected - Math.floor(max / 2), rowsList.length - max));
  const win = rowsList.slice(start, start + max);
  return (
    <Box flexDirection="column">
      {showDeprecated ? <Text dimColor>(showing deprecated)</Text> : null}
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
      {win.map(({ s, prefix }, i) => {
        const sel = start + i === selected;
        const here = s.id === currentId;
        const k = KIND[s.kind] ?? { glyph: "•" };
        // Status dot: busy pulse, unseen result, or "you are here"; else blank.
        const dot = s.busy ? "⋯" : s.unseen ? "●" : here ? "▸" : " ";
        const dotColor = s.busy ? "yellow" : "green";
        const title = (s.title || "(untitled)").replace(k.strip ?? /^\b$/, "");
        return (
          <Box key={s.id} justifyContent="space-between" gap={2}>
            <Text inverse={sel} wrap="truncate">
              <Text color={dotColor} dimColor={dot === " "}>{dot}</Text>{" "}
              <Text dimColor>{prefix}</Text>
              <Text color={k.color} dimColor={!k.color}>{k.glyph}</Text>{" "}
              <Text bold={here} dimColor={!!s.deprecatedAt} strikethrough={!!s.deprecatedAt}>
                {title}
              </Text>
            </Text>
            <Text inverse={sel} dimColor>
              {s.deprecatedAt ? "deprecated" : relTime(s.createdAt)}
            </Text>
          </Box>
        );
      })}
      {rowsList.length === 0 && <Text dimColor>no sessions — ^t creates one</Text>}
      <Text dimColor>
        j/k move · enter open · ^t new · ^x archive · x deprecate · h show hidden
      </Text>
    </Box>
  );
}
