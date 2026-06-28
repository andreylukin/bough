import { api } from "./api.js";
import { clearPastes, closePicker } from "./composer.js";
import { el, esc, toast } from "./dom.js";
import { anyBranchRunning, leafContaining, onActivePath, treeBranches } from "./graph.js";
import { render, renderRunControls } from "./main.js";
import { jumpToEntry, renderMap } from "./map.js";
import { renderHeader, renderRight, renderSidebar, renderTabCounts, switchBranch } from "./panes.js";
import { ACTIVE, state } from "./state.js";
import { closeDrawer, dropSubbar, openDrawer, renderTranscript } from "./transcript.js";

export async function loadSessions() {
  try { state.sessions = await api.sessions(); } catch { state.sessions = []; }
}

// A session link is `sessionId:nodeId` — the session plus a node to focus on.
// The bare `sessionId` (no colon) is still accepted. Node ids never contain a
// colon, so splitting on the first one is unambiguous.
export function parseSessionLink(raw) {
  const s = String(raw || "");
  const i = s.indexOf(":");
  return i === -1 ? { id: s, node: null } : { id: s.slice(0, i), node: s.slice(i + 1) };
}

// Build a link to a node in the current session: `sessionId:nodeId` (or the bare
// session id when there's no node). The inverse of parseSessionLink.
export function sessionLink(node) {
  const sid = state.sessionId || (state.tree && state.tree.id) || "";
  return node ? `${sid}:${node}` : sid;
}

export async function openSession(raw) {
  const { id, node } = parseSessionLink(raw);
  stopPoll();
  closeDrawer();
  closeNav(); // came from the sessions overlay on mobile — return to the transcript
  state.sessionId = id;
  state.viewChildId = null; state.childTree = null; state.childRun = null;
  state.graftRoot = null; state.paneSig = null; state.diff = null; state.files = null; state.mapView = null;
  state.editingEid = null;
  closePicker(); clearPastes();
  try {
    state.tree = await api.tree(id);
    state.run = await api.run(id).catch(() => null);
    state.subagents = await api.subagents(id).catch(() => []);
  } catch (e) { toast(String(e.message || e), true); return; }
  // Reflect the open session in the URL so it's a shareable deep link. A node
  // link keeps the node; a plain open points at the current branch tip.
  syncSessionHash(node || state.tree.active_leaf);
  render();
  ensurePoll();
  if (node) focusNode(node);
}

// Bring `nid` into view: scroll+flash it if it's on the active branch, otherwise
// switch to a branch that contains it first.
export async function focusNode(nid) {
  if (!state.tree || !state.tree.entries.some((e) => e.id === nid)) {
    toast("That turn isn't in this session.", true);
    return;
  }
  if (onActivePath(nid)) { jumpToEntry(nid); return; }
  const leaf = leafContaining(state.tree, nid);
  if (leaf && leaf !== state.tree.active_leaf) await switchBranch(leaf);
  jumpToEntry(nid);
}

// Write the current session (and optional focus node) to the URL hash without
// firing a navigation — `replaceState` doesn't emit `hashchange`, so this won't
// loop back through the hashchange handler.
export function syncSessionHash(node) {
  const want = node ? `${state.sessionId}:${node}` : state.sessionId;
  if (sessionHash() !== want) history.replaceState(null, "", "#" + want);
}

export function sessionHash() {
  try { return decodeURIComponent((location.hash || "").replace(/^#/, "")); }
  catch { return (location.hash || "").replace(/^#/, ""); }
}

// Keep the poller running while the viewed run is active OR any other branch is
// running in the background (so its dot and its turn stay live).

export function ensurePoll() {
  const viewed = state.run && ACTIVE.has(state.run.status);
  if (viewed || anyBranchRunning(state.tree)) startPoll();
}

// Start a fresh session straight in a folder you already have — no form, since
// the project path is the folder header we clicked from.

export async function newSessionInProject(proj) {
  if (!proj) return;
  try {
    const s = await api.createSession(proj);
    await loadSessions();
    await openSession(s.id);
  } catch (e) { toast(String(e.message || e), true); }
}

// A clean in-app form (no native prompt) to start a session in a project dir.

export function newSession() {
  const body = el("div");
  const f = el("div", "field");
  f.appendChild(el("div", "flabel", "project directory"));
  const input = el("input", "fin");
  input.type = "text";
  input.value = state.tree ? state.tree.project : "";
  input.placeholder = "/absolute/path/to/project";
  f.appendChild(input);
  body.appendChild(f);
  body.appendChild(el("div", "hint",
    "bough sandboxes the agent to this directory. The session starts on a fresh branch you can fork as it grows."));
  const create = async () => {
    const p = input.value.trim();
    if (!p) { toast("Enter a project path.", true); input.focus(); return; }
    closeDrawer();
    try {
      const s = await api.createSession(p);
      await loadSessions();
      await openSession(s.id);
    } catch (e) { toast(String(e.message || e), true); }
  };
  input.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); create(); } };
  const acts = el("div", "drawer-actions");
  const btn = el("button", "primary", "Create session");
  btn.onclick = create;
  acts.appendChild(btn);
  openDrawer("new", "New session", body, acts);
  setTimeout(() => input.focus(), 60);
}

// ---- large-paste collapsing ---------------------------------------------
// A big paste (a log, a file) shouldn't bury the composer in a wall of text.
// We intercept it, keep it as a chip, and splice it back in on send.

export async function stopRun() {
  const id = state.viewChildId || state.sessionId;
  if (!id) return;
  try { await api.stop(id); toast("stopping — will halt at the next step"); }
  catch (e) { toast(String(e.message || e), true); }
}

export async function gateDecision(decision, message) {
  const id = state.viewChildId || state.sessionId;
  try {
    await api.control(id, decision, message);
    // Optimistic: flip to running so the spinner shows immediately.
    const run = state.viewChildId ? state.childRun : state.run;
    if (run) run.status = "running";
    state.paneSig = null;
    render();
  } catch (e) { toast(String(e.message || e), true); }
}

export async function forkNode(entryId) {
  try {
    state.tree = await api.fork(state.sessionId, entryId);
    state.run = await api.run(state.sessionId).catch(() => state.run);
    state.graftRoot = null;
    render();
    ensurePoll();
  } catch (e) { toast(String(e.message || e), true); }
}

export async function graftOnto(onto) {
  try {
    state.tree = await api.graft(state.sessionId, state.graftRoot, onto);
    state.graftRoot = null;
    toast("grafted");
    render();
  } catch (e) {
    // Stay armed so the user can pick another parent; say so, since nothing
    // visibly changed.
    toast("graft rejected (cycle or unknown node) — still armed; pick another parent or Cancel", true);
  }
}

export async function toggleGroup(name, on) {
  const cur = new Set(state.tree.groups || []);
  if (on) cur.add(name); else cur.delete(name);
  try {
    await api.setGroups(state.sessionId, [...cur]);
    state.tree = await api.tree(state.sessionId);
    render();
  } catch (e) { toast(String(e.message || e), true); }
}

export async function inspectGroup(name) {
  try {
    const d = await api.groupDetail(name);
    const body = el("div");
    if (d.description) {
      const af = el("div", "field");
      af.appendChild(el("div", "flabel", "about"));
      af.appendChild(el("div", "fval", esc(d.description)));
      body.appendChild(af);
    }
    const f = el("div", "field");
    f.appendChild(el("div", "flabel", `paths (${d.paths.length})`));
    for (const p of d.paths) {
      const row = el("div", "path-row");
      row.innerHTML = `<span class="acc ${esc(p.access)}">${esc(p.access)}</span><span class="pp">${esc(p.path)}</span>`;
      f.appendChild(row);
    }
    body.appendChild(f);
    openDrawer("capability", d.name, body);
  } catch (e) { toast(String(e.message || e), true); }
}

export async function openChild(id) {
  stopPoll();
  closeDrawer();
  state.viewChildId = id;
  state.paneSig = null;
  try {
    state.childTree = await api.tree(id).catch(() => null);
    state.childRun = await api.run(id).catch(() => null);
  } catch (e) { toast(String(e.message || e), true); }
  render();
  startPoll();
}

export function backToParent() {
  state.viewChildId = null; state.childTree = null; state.childRun = null;
  dropSubbar();
  openSession(state.sessionId);
}

// ---- polling -------------------------------------------------------------

export function startPoll() { stopPoll(); state.poll = setInterval(tick, 600); }

export function stopPoll() { if (state.poll) { clearInterval(state.poll); state.poll = null; } }

// A cheap fingerprint of everything the transcript draws, so a poll that returns
// identical state doesn't rebuild the DOM (which caused flicker).

export function runSig(run, subs) {
  if (!run) return "none";
  const last = run.steps && run.steps.length ? JSON.stringify(run.steps[run.steps.length - 1]) : "";
  return [
    run.status,
    run.steps ? run.steps.length : 0,
    run.network ? run.network.length : 0,
    (run.text || "").length,
    last,
    (subs || []).map((s) => s.status).join(","),
  ].join("|");
}

// Per-pane fingerprints. The poller redraws ONLY the panes whose own data
// changed, so a streaming transcript never wipes (and un-clicks) the sidebar,
// the right pane, or the header the user is interacting with.
function paneSigs() {
  const run = state.viewChildId ? state.childRun : state.run;
  const tree = state.tree;
  const header = [
    tree && tree.id, tree && tree.project, tree && tree.model,
    run && run.status, run && run.context_tokens, state.reviewArmed,
    state.config.model,
  ].join("~");
  const sessions = (state.sessions || []).map((s) => [s.id, s.title, s.turns, s.updated].join(",")).join(";");
  const branches = treeBranches(tree).map((b) => [b.leafId, b.active, b.trunk, b.running, b.name, b.turns].join(",")).join(";");
  const sidebar = [
    state.filter, state.openProjects ? [...state.openProjects].sort().join(",") : "",
    state.sessionId, sessions, branches,
  ].join("~");
  const tBox = state.viewChildId ? state.childTree : tree;
  const transcript = [
    state.viewChildId || "", runSig(run, state.subagents),
    tBox && tBox.entries ? tBox.entries.length : 0,
  ].join("~");
  return { header, sidebar, transcript, right: rightSig(run, tree) };
}

// The right pane's signature tracks ONLY the active tab's data, so a background
// change (e.g. the network feed growing while you're on the Tree tab) never
// rebuilds — and un-clicks — the tab you're actually looking at.
function rightSig(run, tree) {
  const tab = state.rightTab;
  let d = "";
  if (tab === "tree") d = [tree && tree.entries && tree.entries.length, tree && tree.active_leaf, state.graftRoot, state.mapShowSuperseded].join(",");
  else if (tab === "changes") d = String(state.diff && tree && state.diff.sessionId === tree.id ? state.diff.files.length : -1);
  else if (tab === "network") d = String(run && run.network ? run.network.length : 0);
  else if (tab === "caps") d = [tree && tree.groups && tree.groups.length, tree && tree.suggested && tree.suggested.length, state.capsFilter, JSON.stringify(state.capsOpen)].join(",");
  else if (tab === "subagents") d = (state.subagents || []).map((s) => s.id + ":" + s.status).join(",") + "|" + state.showDoneSubs;
  return tab + "~" + d;
}

// Poll-driven render: redraw each pane only when its fingerprint changed. The
// run controls are class/attr toggles (no wipe) so they refresh every tick.
function renderPoll() {
  const sig = paneSigs();
  const prev = state.paneSig || {};
  if (sig.header !== prev.header) renderHeader();
  if (sig.sidebar !== prev.sidebar) renderSidebar();
  if (sig.right !== prev.right) renderRight();
  else renderTabCounts(); // keep tab badges live without wiping the body
  if (sig.transcript !== prev.transcript) renderTranscript();
  renderRunControls();
  if (state.mapOpen && (sig.transcript !== prev.transcript || sig.sidebar !== prev.sidebar)) renderMap();
  state.paneSig = sig;
}

export async function tick() {
  const id = state.viewChildId || state.sessionId;
  if (!id) return stopPoll();
  let run;
  try { run = await api.run(id); } catch { return; }

  if (state.viewChildId) {
    state.childRun = run;
    if (!ACTIVE.has(run.status)) { state.childTree = await api.tree(id).catch(() => state.childTree); stopPoll(); }
    renderPoll();
    return;
  }

  state.run = run;
  // Keep subagent statuses fresh while a run is in flight.
  state.subagents = await api.subagents(id).catch(() => state.subagents);
  const viewedDone = !ACTIVE.has(run.status);
  // Refresh the tree when the viewed run finishes OR while any other branch is
  // still running, so its turn lands and the sidebar dots stay live. Keep
  // polling until nothing anywhere is running.
  let othersRunning = anyBranchRunning(state.tree);
  if (viewedDone || othersRunning) {
    state.tree = await api.tree(id).catch(() => state.tree);
    othersRunning = anyBranchRunning(state.tree);
    await loadSessions();
    state.diff = null; // a run may have written files — reload Changes on next view
  }
  if (viewedDone && !othersRunning) stopPoll();
  renderPoll();
}

// ---- collapsible side panes ----------------------------------------------
// Either side pane can be folded away to give the transcript more room; the
// choice persists across reloads (this is a daily driver).

export const PANES_KEY = "bough.panes";

export function applyPanes() {
  let v = {};
  try { v = JSON.parse(localStorage.getItem(PANES_KEY) || "{}"); } catch {}
  document.body.classList.toggle("left-collapsed", !!v.left);
  document.body.classList.toggle("right-collapsed", !!v.right);
}

// Below these widths a side pane is a slide-over overlay (toggled open), not a
// desktop column you collapse: the right pane folds to an overlay sooner (the
// transcript wants the room), the sessions list only on phones.
function isOverlay(side) {
  const w = side === "right" ? 1080 : 760;
  return window.matchMedia(`(max-width: ${w}px)`).matches;
}

export function togglePane(side) {
  if (isOverlay(side)) {
    const cls = "nav-" + side;
    const wasOpen = document.body.classList.contains(cls);
    document.body.classList.remove("nav-left", "nav-right"); // one overlay at a time
    if (!wasOpen) document.body.classList.add(cls);
    return;
  }
  const cls = side + "-collapsed";
  document.body.classList.toggle(cls);
  localStorage.setItem(PANES_KEY, JSON.stringify({
    left: document.body.classList.contains("left-collapsed"),
    right: document.body.classList.contains("right-collapsed"),
  }));
}

// Dismiss any open slide-over pane — on the scrim tap, or after you've picked
// something (so you land back on the transcript).
export function closeNav() {
  document.body.classList.remove("nav-left", "nav-right");
}

// ---- "@" file picker -----------------------------------------------------
// Type "@" in the composer and a fuzzy file picker opens inline over the input.
// Files come from GET /session/:id/files (git-tracked + untracked, .gitignore
// respected), cached per session. ↑/↓ move, Enter/Tab insert, Esc closes.
