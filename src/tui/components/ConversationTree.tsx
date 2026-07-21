import { palette } from "../theme.ts";
import { Box, Text } from "ink";
import type { Message, Part } from "../../schema/parts.ts";
import type { WireSection } from "../api.ts";
import type { TuiSession } from "../store.ts";
import { clip, toolSummary } from "../format.ts";
import { parseSubagentNote } from "../lines.ts";

// The conversation as a branchable tree (pi's /tree model). Each user turn is a
// node; each tool run inside the reply is its own branch point; existing branches
// (forks/compactions/subagents) hang off the turn they split from. Selecting a
// node/tool branches there (a new fork); selecting a branch opens it. bough forks
// into a NEW session, so there is no in-place leaf — the last turn is the live tip.

/** A branch point: a message id and an optional mid-message part cut (a tool run). */
export interface BranchPoint {
  msgId: string;
  /** Cut inside the message — keep parts[0..atPart]. Absent = the whole message. */
  atPart?: number;
}

/** A tool run within a turn: a labeled branch point after that run. */
export interface ToolStep {
  label: string;
  point: BranchPoint;
}

export interface TreeNode {
  msg: Message;
  point: BranchPoint;
  /** Tool runs in the reply, each a branch point ("agree with this one, differ after"). */
  steps: ToolStep[];
  branches: TuiSession[];
  tip: boolean;
  /** All message ids in this turn (user + reply span) — the picks for range ops. */
  msgIds: string[];
}

export type TreeItem =
  | { type: "node"; node: TreeNode; sectionColor?: string }
  | { type: "step"; step: ToolStep; sectionColor?: string }
  | { type: "branch"; session: TuiSession; sectionColor?: string }
  | { type: "section"; section: WireSection; color: string };

/** Sections are topics, not categories — the color only tells adjacent sections
 * apart, so hues cycle by section order (theme-independent, distinct hues). */
export const SECTION_COLORS = [
  "#5c88c9", // blue
  "#4ec98f", // green
  "#d9b45f", // amber
  "#9a7fd1", // iris
  "#e2776e", // red
  "#3fbdb0", // teal
  "#d97a8e", // rose
] as const;

export const sectionColor = (i: number): string => SECTION_COLORS[i % SECTION_COLORS.length];

// The tool runs in a reply span → labeled branch points. atPart cuts through the
// run's result (kept inclusive) so the completed call is retained in the branch.
function stepsOf(span: Message[]): ToolStep[] {
  const steps: ToolStep[] = [];
  for (const m of span) {
    m.parts.forEach((p: Part, i) => {
      if (p.type !== "tool_call") return;
      const raw = p.input as Record<string, unknown> | null | undefined;
      const hint = raw && typeof raw.code === "string"
        ? raw.code.split("\n")[0]
        : raw && typeof raw.command === "string"
        ? raw.command
        : "";
      // Keep through this call's result if present, else through the call itself.
      const resIdx = m.parts.findIndex((q) => q.type === "tool_result" && q.callId === p.id);
      steps.push({
        label: `${p.name}${hint ? ` · ${clip(hint, 44)}` : ""}`,
        point: { msgId: m.id, atPart: resIdx >= 0 ? resIdx : i },
      });
    });
  }
  return steps;
}

export function buildTree(thread: Message[], branches: TuiSession[]): TreeNode[] {
  const nodes: TreeNode[] = [];
  const nodeByMsgId = new Map<string, TreeNode>();
  let cur: { node: TreeNode; span: Message[] } | null = null;
  for (const m of thread) {
    const noteText = m.role === "system"
      ? m.parts.map((p) => ("text" in p ? p.text : "")).join("\n")
      : "";
    if (noteText && parseSubagentNote(noteText)) {
      if (cur) nodeByMsgId.set(m.id, cur.node);
      continue;
    }
    if (m.role === "user") {
      const node: TreeNode = {
        msg: m,
        point: { msgId: m.id },
        steps: [],
        branches: [],
        tip: false,
        msgIds: [m.id],
      };
      cur = { node, span: [] };
      nodes.push(node);
      nodeByMsgId.set(m.id, node);
    } else if (cur) {
      cur.span.push(m);
      cur.node.steps = stepsOf(cur.span);
      cur.node.msgIds.push(m.id);
      nodeByMsgId.set(m.id, cur.node);
    }
  }
  if (nodes.length) nodes[nodes.length - 1].tip = true;
  for (const b of branches) {
    const owner = b.originMessageId ? nodeByMsgId.get(b.originMessageId) : undefined;
    if (owner) owner.branches.push(b);
    else if (nodes.length) nodes[0].branches.push(b);
  }
  for (const n of nodes) n.branches.sort((a, b) => a.createdAt - b.createdAt);
  return nodes;
}

export function treeItems(nodes: TreeNode[], sections?: WireSection[] | null): TreeItem[] {
  const secs = sections ?? [];
  const byStart = new Map(secs.map((s, i) => [s.start, { section: s, color: sectionColor(i) }]));
  const colorAt = (turn: number): string | undefined => {
    const i = secs.findIndex((x) => turn >= x.start && turn <= x.end);
    return i >= 0 ? sectionColor(i) : undefined;
  };
  const items: TreeItem[] = [];
  nodes.forEach((n, turn) => {
    const sec = byStart.get(turn);
    if (sec) items.push({ type: "section", section: sec.section, color: sec.color });
    const color = colorAt(turn);
    items.push({ type: "node", node: n, sectionColor: color });
    for (const s of n.steps) items.push({ type: "step", step: s, sectionColor: color });
    for (const b of n.branches) items.push({ type: "branch", session: b, sectionColor: color });
  });
  return items;
}

/** Inclusive item-index span a section header covers (its nodes + their steps/
 * branches) — what enter on the header arms as the range selection. */
export function sectionSpan(items: TreeItem[], headerIdx: number): [number, number] | null {
  const it = items[headerIdx];
  if (it?.type !== "section") return null;
  let hi = headerIdx;
  for (let i = headerIdx + 1; i < items.length; i++) {
    if (items[i].type === "section") break;
    hi = i;
  }
  return hi > headerIdx ? [headerIdx + 1, hi] : null;
}

const KIND_GLYPH: Record<string, string> = {
  fork: "⑂",
  compaction: "≣",
  subagent: "◆",
  root: "●",
};

export function ConversationTree(
  { items, selected, rows, showDeprecated, range, unadopted }: {
    items: TreeItem[];
    selected: number;
    rows: number;
    showDeprecated: boolean;
    /** Inclusive [lo, hi] item indices highlighted for a range op, or null. */
    range: [number, number] | null;
    /** Subagent session ids with unadopted branch work — their ◆ rows get a
     * marker and the `a adopt` footer key. */
    unadopted: ReadonlySet<string>;
  },
) {
  const max = Math.max(3, rows - 9);
  const start = Math.max(0, Math.min(selected - Math.floor(max / 2), items.length - max));
  const win = items.slice(start, start + max);
  const rangeCount = range
    ? items.slice(range[0], range[1] + 1).filter((it) => it.type === "node").length
    : 0;
  return (
    <Box flexDirection="column">
      <Text dimColor>
        {range
          ? `${rangeCount} turn${
            rangeCount === 1 ? "" : "s"
          } · c compact · e extract · m copy to · d delete (recoverable) · esc`
          : `turn: rewind & edit its message · tool: branch after it · v select range · s sections${
            showDeprecated ? " · (showing deprecated)" : ""
          }`}
      </Text>
      {win.map((it, i) => {
        const idx = start + i;
        const sel = idx === selected;
        const inRange = !!range && idx >= range[0] && idx <= range[1];
        if (it.type === "section") {
          const s = it.section;
          const turns = s.end - s.start + 1;
          return (
            <Text key={`h-${idx}`} inverse={sel} wrap="truncate">
              {" "}
              <Text color={it.color} bold>■ {s.label}</Text>
              <Text dimColor>{`  ${turns} turn${turns === 1 ? "" : "s"}`}</Text>
            </Text>
          );
        }
        // While sections are shown, every turn row carries its section's color
        // as a left gutter bar (range selection overrides it with the accent).
        const gutter = inRange
          ? <Text color={palette.accent}>▍</Text>
          : it.sectionColor
          ? <Text color={it.sectionColor}>▏</Text>
          : <Text>{" "}</Text>;
        if (it.type === "step") {
          return (
            <Text key={`s-${i}`} inverse={sel} wrap="truncate">
              {gutter}{"    "}<Text color={palette.accent}>◇</Text>{" "}
              <Text dimColor>{it.step.label}</Text>
            </Text>
          );
        }
        if (it.type === "branch") {
          const s = it.session;
          const dep = !!s.deprecatedAt;
          return (
            <Text key={`b-${s.id}`} inverse={sel} wrap="truncate">
              {gutter}{"   "}
              <Text
                color={s.kind === "subagent" ? palette.accent : undefined}
                dimColor={s.kind !== "subagent"}
              >
                {KIND_GLYPH[s.kind] ?? "•"}
              </Text>{" "}
              <Text dimColor strikethrough={dep}>
                {(s.title || "(untitled)").replace(/^(fork|compacted|subagent) · /, "")}
              </Text>
              {unadopted.has(s.id) ? <Text color={palette.warn}>{"  unadopted"}</Text> : null}
              {dep ? <Text dimColor>{"  deprecated"}</Text> : null}
            </Text>
          );
        }
        const n = it.node;
        const text = n.msg.parts.find((p) => p.type === "text");
        const preview = text && "text" in text ? clip(text.text.split("\n")[0], 66) : "(no text)";
        return (
          <Text key={`n-${n.msg.id}`} inverse={sel} wrap="truncate">
            {gutter}
            <Text color={palette.info} bold>you</Text> {preview}
            {n.tip ? <Text color={palette.accent}>{"  "}← here</Text> : null}
          </Text>
        );
      })}
      {items.length === 0 && <Text dimColor>no turns yet</Text>}
      <Text dimColor>
        ↑↓ move · enter rewind/branch/open ·{" "}
        {unadopted.size > 0 ? "a adopt · " : ""}x deprecate · h hidden · v select · esc
      </Text>
    </Box>
  );
}
