/**
 * The session tree: every root, fork and subagent branch, in one list.
 *
 * THE INVARIANT THIS HOLDS: **visibility is derived from lineage, never stored.**
 * Spec §4 is explicit — sessions of kind `subagent` and `workflow_agent` collapse
 * under their `originId` and surface only on drill-in; roots and their branches are
 * always listed; and "there is no archive, deprecate, hide, or purge action, and no
 * corresponding columns". The old tree (`src/tui/components/ConversationTree.tsx`)
 * rendered a `deprecatedAt` strikethrough, a `showDeprecated` toggle and an `x
 * deprecate` binding. None of that is ported: the state does not exist, so neither
 * does the affordance. What replaces it is `treeItems` below, which computes the whole
 * visible list from `kind` + `originId` + one set of expanded ids and nothing else.
 *
 * WHY THE COLLAPSE IS A *COUNT*, NOT A HIDE. A fan-out of 40 workflow agents under one
 * turn is the normal case (spec §8), and listing them inline buries the conversation
 * they belong to. But a branch the user cannot see is a branch they cannot reach, so a
 * collapsed origin renders a row that says how many are under it — `⋯ 40 delegated` —
 * rather than nothing. Drill-in expands exactly that node; delegated grandchildren
 * stay collapsed until their own parent is expanded, so opening one fan-out never
 * unfolds the tree beneath it.
 *
 * PURE CORE. `treeItems` is a fold over rows with no React, no clock and no I/O, so the
 * lineage rules are tested by handing it fixture sessions (plan §7: "renderers over
 * fixture state"). The component below is the thin part: it windows the list around the
 * cursor and paints it.
 *
 * NOTE on colour: `tui/theme.ts` (T9.2) has not landed and is not in this task's owned
 * set, so the status hues here are ink's named colours rather than the server-served
 * palette. They are confined to `statusMark`/`KIND_GLYPH` — one record to repoint when
 * the palette arrives. Measurement and clipping DO come from `tui/format.ts`, which has
 * landed: display width is never `String.length`.
 */
import { Box, Text } from "ink";
import type { SessionKind } from "../../schema/parts.ts";
import type { SessionRow } from "../api.ts";
import { clip, fmtUsd, windowAround } from "../format.ts";

// ---------------------------------------------------------------------------
// Lineage (pure)
// ---------------------------------------------------------------------------

/** The kinds that collapse under their origin and surface on drill-in (spec §4). */
export const DELEGATED_KINDS: readonly SessionKind[] = ["subagent", "workflow_agent"];

export function isDelegated(kind: SessionKind): boolean {
  return DELEGATED_KINDS.includes(kind);
}

/** What the tree is built from: the top level, plus whatever drill-in has fetched. */
export interface TreeInput {
  /** `GET /sessions` — roots and their forks/compactions. Never delegated kinds. */
  roots: SessionRow[];
  /** `originId` → `GET /sessions?originId=`. Absent means "not fetched yet". */
  childrenByOrigin: Record<string, SessionRow[]>;
  /** Origins whose delegated children are drilled into. */
  expanded: ReadonlySet<string>;
}

export type TreeItem =
  | {
    type: "session";
    session: SessionRow;
    depth: number;
    /** Delegated children under this session, whether or not they are shown. */
    delegated: number;
    /** Is this node's delegated fan-out currently drilled into? */
    open: boolean;
  }
  /** The collapsed fan-out's own row — reachable, countable, one line. */
  | { type: "collapsed"; originId: string; depth: number; count: number };

function byCreatedAt(a: SessionRow, b: SessionRow): number {
  return a.createdAt - b.createdAt;
}

/**
 * The visible list, depth-first.
 *
 * `seen` is a cycle guard, not a dedupe: `originId` is a pointer the server sets and
 * not a foreign key, so a malformed lineage must render a short tree rather than hang
 * the terminal in an infinite walk.
 */
export function treeItems(input: TreeInput): TreeItem[] {
  const items: TreeItem[] = [];
  const seen = new Set<string>();

  const walk = (session: SessionRow, depth: number): void => {
    if (seen.has(session.id)) return;
    seen.add(session.id);
    const children = [...(input.childrenByOrigin[session.id] ?? [])].sort(byCreatedAt);
    const branches = children.filter((c) => !isDelegated(c.kind));
    const delegated = children.filter((c) => isDelegated(c.kind));
    const open = input.expanded.has(session.id);
    items.push({ type: "session", session, depth, delegated: delegated.length, open });
    // Branches are always listed; delegated children only on drill-in (spec §4).
    for (const child of branches) walk(child, depth + 1);
    if (delegated.length === 0) return;
    if (open) { for (const child of delegated) walk(child, depth + 1); }
    else {
      items.push({
        type: "collapsed",
        originId: session.id,
        depth: depth + 1,
        count: delegated.length,
      });
    }
  };

  for (const root of [...input.roots].sort(byCreatedAt)) {
    // A fork the server also listed at the top level is walked from its origin
    // instead, so it appears once, under what it branched from.
    if (root.originId && input.roots.some((r) => r.id === root.originId)) continue;
    walk(root, 0);
  }
  return items;
}

/** The item a cursor at `selected` is on, or null on an empty tree. */
export function itemAt(items: TreeItem[], selected: number): TreeItem | null {
  return items[selected] ?? null;
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

const KIND_GLYPH: Record<SessionKind, string> = {
  root: "●",
  fork: "⑂",
  compaction: "≣",
  subagent: "◆",
  workflow_agent: "◈",
};

/**
 * Outcome marker. `null` for a session that has never run a turn — an absence, not a
 * state, and rendering a glyph for it would invent one.
 *
 * `outcomeOk === false` is checked ahead of the turn status because it is the
 * DELEGATION outcome (`schema/parts.ts`): a subagent whose turn ended `done` but whose
 * work failed is exactly the branch the tree exists to make findable. There is no
 * acceptance gate behind it — it records that the turn errored, not that a check ran.
 */
export function statusMark(s: SessionRow): { glyph: string; color: string } | null {
  if (s.busy) return { glyph: "⋯", color: "cyan" };
  if (s.outcomeOk === false) return { glyph: "✗", color: "red" };
  switch (s.lastTurnStatus) {
    case "running":
      return { glyph: "⋯", color: "cyan" };
    case "orphaned":
    case "interrupted":
      return { glyph: "◼", color: "yellow" };
    case "error":
      return { glyph: "✗", color: "red" };
    case "done":
      return { glyph: "✓", color: "green" };
    default:
      return null;
  }
}

export function titleOf(s: SessionRow): string {
  return (s.title || "(untitled)").replace(/^(fork|compacted|subagent|workflow) · /, "");
}

export function Tree(
  { items, selected, rows }: { items: TreeItem[]; selected: number; rows: number },
) {
  const height = Math.max(3, rows - 4);
  const { start, end } = windowAround(selected, items.length, height);
  const window = items.slice(Math.max(0, start), end);
  return (
    <Box flexDirection="column">
      <Text dimColor wrap="truncate">
        ↑↓ move · ⏎ open · → drill into delegated work · esc back
      </Text>
      {items.length === 0 ? <Text dimColor>no sessions yet</Text> : null}
      {window.map((item, i) => {
        const idx = Math.max(0, start) + i;
        const sel = idx === selected;
        const cursor = sel ? "❯ " : "  ";
        const indent = "  ".repeat(item.depth);
        if (item.type === "collapsed") {
          return (
            <Text key={`c-${item.originId}`} wrap="truncate">
              <Text color={sel ? "cyan" : undefined}>{cursor}</Text>
              <Text dimColor>
                {indent}⋯ {item.count} delegated · → drill in
              </Text>
            </Text>
          );
        }
        const s = item.session;
        const mark = statusMark(s);
        return (
          <Text key={s.id} wrap="truncate">
            <Text color={sel ? "cyan" : undefined}>{cursor}</Text>
            <Text>{indent}</Text>
            <Text dimColor={!isDelegated(s.kind)}>{KIND_GLYPH[s.kind]}</Text>
            {mark ? <Text color={mark.color}>{` ${mark.glyph}`}</Text> : <Text>{"  "}</Text>}
            <Text bold={sel}>{" "}{clip(titleOf(s), 52 - item.depth * 2)}</Text>
            {item.delegated > 0
              ? (
                <Text dimColor>
                  {"  "}
                  {item.open ? "▾" : "▸"} {item.delegated}
                </Text>
              )
              : null}
            {s.costUsd ? <Text dimColor>{`  ${fmtUsd(s.costUsd)}`}</Text> : null}
          </Text>
        );
      })}
    </Box>
  );
}
