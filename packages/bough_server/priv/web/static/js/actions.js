import { api } from "./api.js";
import { clearPastes, closePicker } from "./composer.js";
import { el, esc, toast } from "./dom.js";
import { anyBranchRunning } from "./graph.js";
import { render } from "./main.js";
import { ACTIVE, state } from "./state.js";
import { closeDrawer, dropSubbar, openDrawer } from "./transcript.js";

export async function loadSessions() {
  try { state.sessions = await api.sessions(); } catch { state.sessions = []; }
}

export async function openSession(id) {
  stopPoll();
  closeDrawer();
  state.sessionId = id;
  state.viewChildId = null; state.childTree = null; state.childRun = null;
  state.graftRoot = null; state.lastSig = null; state.diff = null; state.files = null;
  closePicker(); clearPastes();
  try {
    state.tree = await api.tree(id);
    state.run = await api.run(id).catch(() => null);
    state.subagents = await api.subagents(id).catch(() => []);
  } catch (e) { toast(String(e.message || e), true); return; }
  render();
  ensurePoll();
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
    state.lastSig = null;
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
  } catch (e) { toast("graft rejected (cycle or unknown node)", true); }
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
  state.lastSig = null;
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

// A cheap fingerprint of everything the transcript/right pane draws, so a poll
// that returns identical state doesn't rebuild the DOM (which caused flicker).

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

export async function tick() {
  const id = state.viewChildId || state.sessionId;
  if (!id) return stopPoll();
  let run;
  try { run = await api.run(id); } catch { return; }

  if (state.viewChildId) {
    state.childRun = run;
    const done = !ACTIVE.has(run.status);
    if (done) { state.childTree = await api.tree(id).catch(() => state.childTree); stopPoll(); }
    const sig = runSig(run, []) + "|c";
    if (done || sig !== state.lastSig) { state.lastSig = sig; render(); }
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
  const sig = runSig(run, state.subagents) + "|" + (othersRunning ? "b" : "");
  if (viewedDone || othersRunning || sig !== state.lastSig) { state.lastSig = sig; render(); }
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

export function togglePane(side) {
  const cls = side + "-collapsed";
  document.body.classList.toggle(cls);
  localStorage.setItem(PANES_KEY, JSON.stringify({
    left: document.body.classList.contains("left-collapsed"),
    right: document.body.classList.contains("right-collapsed"),
  }));
}

// ---- "@" file picker -----------------------------------------------------
// Type "@" in the composer and a fuzzy file picker opens inline over the input.
// Files come from GET /session/:id/files (git-tracked + untracked, .gitignore
// respected), cached per session. ↑/↓ move, Enter/Tab insert, Esc closes.
