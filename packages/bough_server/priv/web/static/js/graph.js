import { clip } from "./dom.js";
import { MAP } from "./map.js";
import { state } from "./state.js";

export function activePath(tree) {
  if (!tree) return [];
  const byId = new Map(tree.entries.map((e) => [e.id, e]));
  const path = [];
  let cur = byId.get(tree.active_leaf);
  while (cur) {
    path.push(cur);
    cur = cur.parent_id ? byId.get(cur.parent_id) : null;
  }
  return path.reverse();
}

// ---- rendering: transcript ----------------------------------------------

export const parseStep = (content) => { try { return JSON.parse(content); } catch { return {}; } };

export function onActivePath(eid) { return activePath(state.tree).some((e) => e.id === eid); }

// The tool-call rows shown when a turn is expanded — mirroring what the
// transcript actually renders (call+exec merged, empty/duplicate plans dropped)
// so every row's `eid` matches a real `[data-eid]` anchor to jump to.

export function turnStepRows(stepEntries, turnEntry) {
  const ans = (turnEntry.content || "").trim();
  const ps = stepEntries.map((en) => ({ en, s: parseStep(en.content) }));
  const rows = [];
  for (let i = 0; i < ps.length; i++) {
    const { en, s } = ps[i], nx = ps[i + 1];
    if (s.type === "plan" || s.type === "text") {
      const txt = (s.text || "").trim();
      if (!txt || txt === ans) continue; // empty, or the trailing plan == the reply
      rows.push({ eid: en.id, type: "plan", tag: "plan", label: txt });
    } else if (s.type === "call" && nx && nx.s.type === "exec") {
      rows.push({ eid: en.id, type: "call", tag: s.verb || "call", label: s.arg || s.detail || "" });
      i++; // the exec is folded into this row, as in the transcript
    } else if (s.type === "call" || s.type === "exec") {
      rows.push({ eid: en.id, type: s.type, tag: s.verb || s.type, label: s.arg || s.detail || "" });
    } else if (s.type === "check") {
      rows.push({ eid: en.id, type: "check", tag: s.ok ? "✓" : "✗", label: "check" });
    } else if (s.type === "worker") {
      rows.push({ eid: en.id, type: "worker", tag: "worker", label: s.command || "" });
    }
  }
  return rows;
}

// Jump the main transcript to a turn or tool-call: close the map, scroll its
// anchor into view, and flash it. Only the active branch is rendered, so a node
// on another branch can't be scrolled to — nudge toward Fork instead.

export function visibleGraph(tree, showSup) {
  const sup = new Set(showSup ? [] : (tree.superseded || []));
  const byId = new Map(tree.entries.map((e) => [e.id, e]));
  const children = new Map();
  for (const e of tree.entries) {
    if (!children.has(e.parent_id)) children.set(e.parent_id, []);
    children.get(e.parent_id).push(e);
  }
  const isVisible = (n) => n.role === "user" || n.role === "assistant";
  const vnodes = new Map();      // id -> { id, entry, parent, steps: [stepEntry] }
  const vchildren = new Map();   // visible-parent-id (or null) -> [child id]
  const roots = tree.entries.filter((e) => !e.parent_id || !byId.has(e.parent_id));
  // `pending` carries the tool_result steps seen since the last visible node, so
  // each turn owns the steps that produced it (kept per-branch — a fork resets
  // pending down each child path).
  const walk = (node, vparent, pending) => {
    if (sup.has(node.id)) return;
    if (isVisible(node)) {
      vnodes.set(node.id, { id: node.id, entry: node, parent: vparent, steps: pending });
      if (!vchildren.has(vparent)) vchildren.set(vparent, []);
      vchildren.get(vparent).push(node.id);
      (children.get(node.id) || []).forEach((c) => walk(c, node.id, []));
    } else {
      const next = node.role === "tool_result" ? pending.concat(node) : pending;
      (children.get(node.id) || []).forEach((c) => walk(c, vparent, next));
    }
  };
  roots.forEach((r) => walk(r, null, []));
  return { vnodes, vchildren };
}

// Trunk-and-branches layout (not a centered/symmetric tidy tree): the "main
// bough" runs straight down a single lane, and every fork sends its secondary
// children off into fresh lanes to the right. The bough is the child that
// carries the active branch, or — failing that — the deepest line, so it stays
// stable and reads like a tree with one main limb.
// Order a node's visible children with the "main bough" first — the child on
// the active branch, else the deepest line. Used by both the map layout and the
// outline so a linear chain stays straight and only real forks branch off.

export function branchOrder(vchildren, tree) {
  const activeIds = new Set(activePath(tree).map((e) => e.id));
  const depthMemo = new Map();
  const subDepth = (id) => {
    if (depthMemo.has(id)) return depthMemo.get(id);
    const kids = vchildren.get(id) || [];
    const d = kids.length ? 1 + Math.max(...kids.map(subDepth)) : 0;
    depthMemo.set(id, d);
    return d;
  };
  const score = (id) => (activeIds.has(id) ? 1e6 : 0) + subDepth(id);
  return (id) => {
    const kids = (vchildren.get(id) || []).slice();
    if (kids.length <= 1) return kids;
    let best = 0;
    for (let i = 1; i < kids.length; i++) if (score(kids[i]) > score(kids[best])) best = i;
    return [kids[best], ...kids.filter((_, i) => i !== best)];
  };
}

// A "branch" is a live leaf of the conversation tree — its own continuable
// thread sharing a common prefix with its siblings. Phase 1 derives them from
// the visible graph; switching is a fork (set-leaf + snapshot restore), and
// continuing appends to whichever leaf is active.

export function treeBranches(tree) {
  if (!tree || !tree.entries || !tree.entries.length) return [];
  const { vnodes, vchildren } = visibleGraph(tree, false);
  const activeIds = new Set(activePath(tree).map((e) => e.id));
  const leaves = [...vnodes.values()].filter((n) => !(vchildren.get(n.id) || []).length);
  // "Trunk" = the branch whose files are on disk. If the stored trunk pointer
  // isn't a current leaf (e.g. it was forked into an interior node), fall back to
  // the active branch so exactly one branch is always marked — adopt re-pins it.
  const trunkIsLeaf = leaves.some((n) => n.id === tree.trunk_leaf);
  const trunkLeaf = trunkIsLeaf ? tree.trunk_leaf : tree.active_leaf;
  const branches = leaves.map((n) => {
    // Count user prompts (= conversation turns), not every visible node, so the
    // branch count agrees with the sidebar's "N turns".
    let turns = 0, cur = n;
    while (cur) { if (cur.entry.role === "user") turns++; cur = cur.parent ? vnodes.get(cur.parent) : null; }
    return {
      leafId: n.id,
      name: (n.entry.label || "").trim() || clip(n.entry.content, 38) || "branch",
      named: !!(n.entry.label || "").trim(),
      turns,
      active: activeIds.has(n.id),
      trunk: n.id === trunkLeaf,
      // A pending user-entry tip means a run is in flight on this branch (the
      // turn lands in the tree only on completion).
      running: n.entry.role === "user",
    };
  });
  // Disambiguate auto-named branches that collide (same leaf content), so two
  // rows aren't indistinguishable; explicitly-named branches are left alone.
  const counts = {};
  for (const b of branches) counts[b.name] = (counts[b.name] || 0) + 1;
  const seen = {};
  for (const b of branches) {
    if (!b.named && counts[b.name] > 1) {
      seen[b.name] = (seen[b.name] || 0) + 1;
      b.name = `${b.name} (${seen[b.name]})`;
    }
  }
  branches.sort((a, b) => (a.active === b.active ? 0 : a.active ? -1 : 1));
  return branches;
}

// True while any branch has a run in flight, so the poller keeps the sidebar
// dots and the tree live even when you've navigated to an idle branch.

export function anyBranchRunning(tree) {
  return treeBranches(tree).some((b) => b.running);
}

// Switch the active branch to a leaf — reuses fork (set active_leaf + restore
// that leaf's filesystem snapshot). No-op if it's already the active branch.

export function layoutGraph(vchildren, tree) {
  const ordered = branchOrder(vchildren, tree);
  const pos = new Map();
  let maxLane = -1;
  // The primary child inherits the parent's lane (straight down); each later
  // sibling claims a brand-new lane to the right. Because the primary subtree is
  // laid out first, those new lanes never collide with it.
  const place = (id, lane, depth) => {
    if (lane > maxLane) maxLane = lane;
    pos.set(id, { x: lane * MAP.X, y: depth * MAP.Y });
    ordered(id).forEach((k, i) => place(k, i === 0 ? lane : ++maxLane, depth + 1));
  };
  (vchildren.get(null) || []).forEach((r, i) => place(r, i === 0 ? 0 : ++maxLane, 0));
  return pos;
}

export function nodeLabel(id) {
  const e = state.tree.entries.find((x) => x.id === id);
  return e ? (e.role === "tool_result" ? stepLabel(e.content) : e.content) : id;
}

export function stepLabel(content) {
  try {
    const s = JSON.parse(content);
    if (s.type === "call" || s.type === "exec") return `${s.verb} ${s.arg || ""}`;
    if (s.type === "check") return s.ok ? "CHECK ✓" : "CHECK ✗";
    if (s.type === "plan" || s.type === "text") return s.text;
    return s.type;
  } catch { return content; }
}

// ---- Changes (diff review) ----------------------------------------------
// The agent writes straight to the workspace; this is the review surface — the
// uncommitted diff (modified + new files) so you can see what it actually did
// before keeping it. Lazy-loaded and cached per session in state.diff.
