import { Box, Text } from "ink";
import type { Message } from "../../schema/parts.ts";
import type { TuiSession } from "../store.ts";
import { clip, toolSummary } from "../format.ts";
import { parseSubagentNote } from "../lines.ts";

// The conversation as a branchable tree: each user message is a node, with a dim
// one-line summary of the reply it drew and any branches (forks/compactions/
// subagents) that split off within that turn. Matches pi's /tree — select a node to
// branch there. bough branches into a NEW session (fork), so there is no in-place
// leaf; the last node is the live tip.

export interface TreeNode {
  /** The user message this node branches from. */
  msg: Message;
  /** One-line summary of the assistant's reply to it. */
  reply: string;
  /** Branch sessions that split off during this turn. */
  branches: TuiSession[];
  /** True for the final user turn — the conversation's live tip. */
  tip: boolean;
}

/** A flat, selectable row: a message node or one of its branch children. */
export type TreeItem =
  | { type: "node"; node: TreeNode }
  | { type: "branch"; session: TuiSession };

function summarizeReply(msgs: Message[]): string {
  const calls = msgs.flatMap((m) => toolSummary(m.parts).calls);
  const text = msgs.flatMap((m) => m.parts).find((p) => p.type === "text");
  const bits: string[] = [];
  if (calls.length) bits.push(`${calls.length} tool ${calls.length === 1 ? "call" : "calls"}`);
  if (text && "text" in text) bits.push("replied");
  return bits.join(" · ") || "…";
}

// Build the node list: group each user turn with its reply span, attach branch
// sessions to the turn whose messages include their originMessageId.
export function buildTree(thread: Message[], branches: TuiSession[]): TreeNode[] {
  const nodes: TreeNode[] = [];
  const nodeByMsgId = new Map<string, TreeNode>(); // every msg id → its owning turn
  let cur: { node: TreeNode; span: Message[] } | null = null;
  for (const m of thread) {
    const noteText = m.role === "system"
      ? m.parts.map((p) => ("text" in p ? p.text : "")).join("\n")
      : "";
    // Subagent completion notes are represented by their branch, not a node — but
    // still map their id so a branch originating from the note attaches to this turn.
    if (noteText && parseSubagentNote(noteText)) {
      if (cur) nodeByMsgId.set(m.id, cur.node);
      continue;
    }
    if (m.role === "user") {
      const node: TreeNode = { msg: m, reply: "", branches: [], tip: false };
      cur = { node, span: [] };
      nodes.push(node);
      nodeByMsgId.set(m.id, node);
    } else if (cur) {
      cur.span.push(m);
      cur.node.reply = summarizeReply(cur.span);
      nodeByMsgId.set(m.id, cur.node);
    }
  }
  if (nodes.length) nodes[nodes.length - 1].tip = true;
  for (const b of branches) {
    const owner = b.originMessageId ? nodeByMsgId.get(b.originMessageId) : undefined;
    if (owner) owner.branches.push(b);
    else if (nodes.length) nodes[0].branches.push(b); // orphan origin → first turn
  }
  // Branches oldest-first within a turn.
  for (const n of nodes) n.branches.sort((a, b) => a.createdAt - b.createdAt);
  return nodes;
}

// Flatten to the selectable row list the picker navigates.
export function treeItems(nodes: TreeNode[]): TreeItem[] {
  const items: TreeItem[] = [];
  for (const n of nodes) {
    items.push({ type: "node", node: n });
    for (const b of n.branches) items.push({ type: "branch", session: b });
  }
  return items;
}

const KIND_GLYPH: Record<string, string> = {
  fork: "⑂",
  compaction: "≣",
  subagent: "◆",
  root: "●",
};

export function ConversationTree(
  { items, selected, rows }: { items: TreeItem[]; selected: number; rows: number },
) {
  const max = Math.max(3, rows - 8);
  const start = Math.max(0, Math.min(selected - Math.floor(max / 2), items.length - max));
  const win = items.slice(start, start + max);
  return (
    <Box flexDirection="column" borderStyle="round" borderColor="gray" paddingX={1}>
      <Text bold>conversation · branch at any turn</Text>
      {win.map((it, i) => {
        const sel = start + i === selected;
        if (it.type === "branch") {
          const s = it.session;
          const g = KIND_GLYPH[s.kind] ?? "•";
          return (
            <Text key={`b-${s.id}`} inverse={sel} wrap="truncate">
              {"    "}
              <Text
                color={s.kind === "subagent" ? "green" : undefined}
                dimColor={s.kind !== "subagent"}
              >
                {g}
              </Text>{" "}
              <Text dimColor>
                {(s.title || "(untitled)").replace(/^(fork|compacted|subagent) · /, "")}
              </Text>
            </Text>
          );
        }
        const n = it.node;
        const text = n.msg.parts.find((p) => p.type === "text");
        const preview = text && "text" in text ? clip(text.text.split("\n")[0], 70) : "(no text)";
        return (
          <Box key={`n-${n.msg.id}`} flexDirection="column">
            <Text inverse={sel} wrap="truncate">
              <Text color="cyan" bold>you</Text> {preview}
              {n.tip ? <Text color="green">{"  "}← here</Text> : null}
            </Text>
            <Text dimColor wrap="truncate">{"      ↳ "}{n.reply}</Text>
          </Box>
        );
      })}
      {items.length === 0 && <Text dimColor>no turns yet</Text>}
      <Text dimColor>↑↓ move · enter branch here / open · esc close</Text>
    </Box>
  );
}
