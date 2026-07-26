import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import { SelRow } from "./SelRow.tsx";
import { relTime, sessionLabel, windowAround } from "../format.ts";
import type { TuiSession } from "../store.ts";

export interface TreeRow {
  s: TuiSession;
  depth: number;
  /** Box-drawing connector prefix (│ ├ └) drawn to this row's glyph. */
  prefix: string;
}

const VISIBLE_KINDS = new Set(["root", "fork", "compaction", "subagent"]);

// A session's lineage parent is what it BRANCHED FROM: originId (forks, compactions,
// subagents, extracted roots all record it). parentId is a legacy sibling field,
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

// Ported verbatim from ConversationTree's subagentMark — the picker showed a
// blank dot for every finished subagent, so the one view you scan to find "what
// went wrong" was the one view that wouldn't say. Both views must agree on the
// vocabulary, so keep this in step with ConversationTree.tsx.
function subagentMark(s: TuiSession): { glyph: string; color: string } | null {
  if (s.kind !== "subagent") return null;
  if (s.busy) return { glyph: "⋯", color: palette.warn };
  if (s.lastTurnStatus === "interrupted") return { glyph: "◼", color: palette.warn };
  if (s.lastTurnStatus === "orphaned") return { glyph: "◼", color: palette.warn };
  if (s.lastTurnStatus === "error" || s.outcomeOk === false) {
    return { glyph: "✗", color: palette.error };
  }
  if (s.outcomeOk === true && s.outcomeCheckPassed === false) {
    return { glyph: "✓!", color: palette.warn };
  }
  if (s.lastTurnStatus === "done" || s.outcomeOk === true) {
    return { glyph: "✓", color: palette.accent };
  }
  return null; // never ran / legacy row — no marker
}

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
  const max = Math.max(3, rows - 11); // -11, not -10: the outcome legend adds a line
  const { start } = windowAround(selected, rowsList.length, max);
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
        ? (
          <Text dimColor>
            (showing hidden — deprecated + archived · u restores an archived row)
          </Text>
        )
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
        // Status dot: busy pulse or unseen result; else blank. "you are here" used
        // to live here too, but a "▸" in the cursor column competed with the real
        // selection (an unglyphed inverse bar) — and x archives the SELECTION.
        // The cursor is now "❯" (matching the workflow list) and here is a right tag.
        const dot = s.busy ? "⋯" : s.unseen ? "●" : " ";
        const dotColor = s.busy ? palette.warn : palette.accent;
        const mark = subagentMark(s);
        // Untitled rows fall back to the workspace basename, never a raw uuid.
        const title = sessionLabel((s.title || "").replace(k.strip ?? /^\b$/, ""), s.workspace);
        // Project-dir basename — two sessions on different projects were
        // indistinguishable by title alone (user-testing). originDir is the
        // stable project path (mirrors workspace, which never moves).
        const dir = s.originDir?.split("/").pop() ?? null;
        return (
          // Selected rows drop custom span colors: under inverse a colored fg
          // becomes a colored bg speck inside the light bar.
          <SelRow
            key={s.id}
            sel={sel}
            right={
              <Text dimColor>
                {here ? "here  " : ""}
                {s.archivedAt ? "archived" : s.deprecatedAt ? "deprecated" : relTime(s.createdAt)}
              </Text>
            }
          >
            <Text color={sel ? undefined : palette.accent}>{sel ? "❯" : " "}</Text>
            <Text color={sel ? undefined : dotColor} dimColor={dot === " "}>{dot}</Text>{" "}
            <Text dimColor>{prefix}</Text>
            <Text color={k.accent && !sel ? palette.accent : undefined} dimColor={!k.accent}>
              {k.glyph}
            </Text>{" "}
            <Text
              bold={here}
              dimColor={!!s.deprecatedAt || !!s.archivedAt}
              strikethrough={!!s.deprecatedAt}
            >
              {title}
            </Text>
            {mark ? <Text color={sel ? undefined : mark.color}>{` ${mark.glyph}`}</Text> : null}
            {dir ? <Text dimColor>{"  "}{dir}</Text> : null}
          </SelRow>
        );
      })}
      {rowsList.length === 0 && <Text dimColor>no sessions — n creates one</Text>}
      <Text dimColor>● root · ⑂ fork · ◆ subagent · ≣ compacted · ⋯ running</Text>
      <Text dimColor>outcome: ✓ done · ✓! check failed · ✗ failed · ◼ interrupted</Text>
      <Text dimColor>
        ⇥ tab · j/k move · / filter · enter open · n new · x archive · D deprecate · h show hidden ·
        u restore
      </Text>
    </Box>
  );
}
