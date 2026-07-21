import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
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

// Per-kind glyph + how to clean the auto-generated title prefix. `accent: true`
// resolves to palette.accent at render time (a hex here would freeze the boot
// palette — the theme can change mid-run).
const KIND: Record<string, { glyph: string; accent?: boolean; strip?: RegExp }> = {
  root: { glyph: "●", accent: true },
  fork: { glyph: "⑂", strip: /^fork · / },
  compaction: { glyph: "≣", strip: /^compacted · / },
  subagent: { glyph: "◆", accent: true, strip: /^subagent · / },
};

export function SessionPicker(
  { rowsList, selected, filter, filterActive, rows, currentId, showDeprecated, moveHint, msg }: {
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
    /** True while picking a destination for a copy-to — Enter appends instead of opens. */
    moveHint: boolean;
    /** Transient feedback line (e.g. why a keypress didn't apply). */
    msg: string | null;
  },
) {
  const max = Math.max(3, rows - 10);
  const start = Math.max(0, Math.min(selected - Math.floor(max / 2), rowsList.length - max));
  const win = rowsList.slice(start, start + max);
  return (
    <Box flexDirection="column">
      {moveHint
        ? (
          <Text color={palette.accent}>
            ▸ copy here: pick a destination · enter appends the turns · esc cancels
          </Text>
        )
        : msg
        ? <Text color={palette.warn}>{msg}</Text>
        : showDeprecated
        ? <Text dimColor>(showing deprecated)</Text>
        : null}
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
        const dotColor = s.busy ? palette.warn : palette.accent;
        const title = (s.title || "(untitled)").replace(k.strip ?? /^\b$/, "");
        return (
          <Box key={s.id} justifyContent="space-between" gap={2}>
            <Text inverse={sel} wrap="truncate">
              <Text color={dotColor} dimColor={dot === " "}>{dot}</Text>{" "}
              <Text dimColor>{prefix}</Text>
              <Text color={k.accent ? palette.accent : undefined} dimColor={!k.accent}>
                {k.glyph}
              </Text>{" "}
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
      {rowsList.length === 0 && <Text dimColor>no sessions — n creates one</Text>}
      <Text dimColor>● root · ⑂ fork · ◆ subagent · ≣ compacted</Text>
      <Text dimColor>
        j/k move · / filter · enter open · n new · ^x archive · x deprecate · h show hidden
      </Text>
    </Box>
  );
}
