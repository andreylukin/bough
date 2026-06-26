import { closeNav, forkNode, graftOnto, inspectGroup, toggleGroup } from "./actions.js";
import { api } from "./api.js";
import { $, ago, clip, el, esc, humanTok, projBase, toast } from "./dom.js";
import { branchOrder, nodeLabel, treeBranches, visibleGraph } from "./graph.js";
import { render } from "./main.js";
import { openMap } from "./map.js";
import { ACTIVE, state } from "./state.js";
import { closeDrawer, inspectNet, inspectNode, openDrawer } from "./transcript.js";

export function renderHeader() {
  renderModelPicker();
  const ctx = $("#ctx");
  if (!state.tree) { ctx.innerHTML = "no session"; }
  else {
    const st = state.run ? state.run.status : "idle";
    const cls = ACTIVE.has(st) ? (st === "running" ? "running" : "awaiting") : st;
    const meter = contextMeter(state.run ? state.run.context_tokens : 0, effectiveModel());
    const full = state.tree.project || "";
    const base = full.split("/").filter(Boolean).pop() || full;
    ctx.innerHTML =
      `<span class="proj" title="${esc(full)}">${esc(base)}</span> ` +
      `<span class="sid" data-act="copy-id" data-id="${esc(state.tree.id)}" ` +
      `title="Click to copy session id">${esc(state.tree.id)}</span> ` +
      `<span class="badge ${cls}">${esc(st)}</span>${meter}`;
  }
  $("#review-toggle").checked = state.reviewArmed;
}

// The model this session's runs will use: its per-session override if pinned,
// else the server's env default.
export function effectiveModel() {
  return (state.tree && state.tree.model) || state.config.model;
}

// Header model picker: pin the supervisor model (and its provider) for the open
// session, spanning every provider, or fall back to the server default. With no
// session open there's nothing to pin to, so it shows the default read-only.
export function renderModelPicker() {
  const box = $("#model");
  box.innerHTML = "";
  const def = state.models.default || { provider: state.config.provider, model: state.config.model };
  if (!def.provider) return;
  if (!state.tree) {
    box.textContent = `${def.provider} · ${def.model}`;
    return;
  }
  // "|"-joined provider/model identifies a selection; "" means inherit default.
  const curP = state.tree.provider || "";
  const curM = state.tree.model || "";
  const curVal = curP && curM ? `${curP}|${curM}` : "";
  const sel = el("select", "model-select");
  sel.title = "Supervisor provider + model for this session";
  sel.appendChild(option("", `${def.provider} · ${def.model} (default)`, !curVal));
  let matched = false;
  for (const g of (state.models.providers || [])) {
    const grp = el("optgroup", "");
    grp.label = g.provider;
    for (const m of g.models) {
      const v = `${g.provider}|${m}`;
      const on = v === curVal;
      matched = matched || on;
      grp.appendChild(option(v, m, on));
    }
    sel.appendChild(grp);
  }
  // A pin that isn't in any provider's curated list still shows, selected.
  if (curVal && !matched) sel.appendChild(option(curVal, `${curP} · ${curM}`, true));
  sel.onchange = () => pickModel(sel.value);
  box.appendChild(sel);
}

function option(value, label, selected) {
  const o = el("option", "", esc(label));
  o.value = value;
  o.selected = selected;
  return o;
}

async function pickModel(value) {
  if (!state.sessionId) return;
  const [provider, model] = value ? value.split("|") : [null, null];
  try {
    state.tree = await api.setModel(state.sessionId, provider, model);
    toast(model ? `Model → ${provider} · ${model}` : "Model → default");
    renderHeader();
  } catch (e) { toast(String(e.message || e), true); }
}

// Context-window gauge: how full the model's context is after the last turn.
// `context_tokens` is the last turn's input+output (engine.gleam); the window is
// estimated per model family (override exact value isn't worth a round-trip).

export function contextMeter(tokens, model) {
  if (!tokens) return "";
  const win = contextWindow(model);
  const pct = Math.min(100, Math.round((tokens / win) * 100));
  const lvl = pct >= 85 ? "hot" : pct >= 60 ? "warm" : "ok";
  return (
    ` <span class="ctxmeter ${lvl}" title="${tokens.toLocaleString()} of ~${win.toLocaleString()} context tokens used (last turn)">` +
    `<span class="ctxbar"><span class="ctxfill" style="width:${pct}%"></span></span>` +
    `<span class="ctxnum">${humanTok(tokens)}/${humanTok(win)} · ${pct}%</span></span>`
  );
}

export function contextWindow(model) {
  const m = (model || "").toLowerCase();
  if (/gemini/.test(m)) return 1000000;
  if (/claude/.test(m)) return 200000;
  if (/glm/.test(m)) return 200000;
  if (/gpt-5|gpt-4\.1|o[1-4]\b/.test(m)) return 200000;
  if (/gpt-4o|gpt-4/.test(m)) return 128000;
  return 128000; // llama/mistral/qwen/deepseek and unknowns
}

export function renderSidebar() {
  const box = $("#sessions");
  box.innerHTML = "";
  const q = (state.filter || "").toLowerCase();
  const sessions = q
    ? state.sessions.filter((s) =>
        (s.title || "").toLowerCase().includes(q) || (s.project || "").toLowerCase().includes(q))
    : state.sessions;
  if (sessions.length === 0) {
    box.appendChild(el("div", "hint", q ? "No sessions match." : "No sessions yet."));
    return;
  }

  // Group by project, preserving the server's recency order (groups ranked by
  // their most-recent session, sessions within a group by recency).
  const groups = [];
  const byProj = new Map();
  for (const s of sessions) {
    const key = s.project || "";
    if (!byProj.has(key)) { byProj.set(key, []); groups.push(key); }
    byProj.get(key).push(s);
  }

  // The active session's project is the one you're working in: expand it by
  // default, collapse the rest. A search query expands every match.
  const activeProj = state.tree ? state.tree.project : null;
  const isOpen = (key) =>
    q ? true : (state.openProjects ? state.openProjects.has(key) : key === activeProj);

  const item = (s) => {
    const it = el("div", "session-item" + (s.id === state.sessionId ? " active" : ""));
    it.dataset.act = "open-session";
    it.dataset.id = s.id;
    it.appendChild(el("div", "title", esc(clip(s.title || "untitled task", 60))));
    it.appendChild(el("div", "meta",
      `<span>${s.turns} turn${s.turns === 1 ? "" : "s"} · ${ago(s.updated)}</span>`));
    return it;
  };

  // One row per branch (live leaf), nested under the session you're viewing, so
  // forked conversations show side by side. Click to switch the active branch.
  const branchRow = (b) => {
    const r = el("div", "branch-item" + (b.active ? " active" : "") + (b.named ? " named" : "") + (b.trunk ? " trunk" : "") + (b.running ? " running" : ""));
    r.dataset.act = "switch-branch";
    r.dataset.leaf = b.leafId;
    // The trunk branch is on disk; others get an "adopt" button to bring their
    // files into the project dir (and become trunk). A running branch shows a
    // spinner instead (you can't adopt mid-run).
    const tail = b.running
      ? `<span class="bspin" title="running">◍</span>`
      : b.trunk
        ? `<span class="btrunk" title="the project dir reflects this branch">trunk</span>`
        : `<button class="badopt" data-act="adopt-branch" data-leaf="${esc(b.leafId)}" title="restore the project dir to this branch and make it trunk">adopt</button>`;
    r.innerHTML =
      `<span class="bconn">${b.active ? "●" : "○"}</span>` +
      `<span class="bname" title="double-click to rename">${esc(b.name)}</span>` +
      `<span class="bmeta">${b.turns}</span>` + tail;
    // Double-click the name to rename the branch inline (Enter saves, Esc cancels).
    r.querySelector(".bname").ondblclick = (ev) => { ev.stopPropagation(); renameBranch(r, b); };
    return r;
  };

  for (const key of groups) {
    const list = byProj.get(key);
    const open = isOpen(key);
    const head = el("div", "proj-head" + (open ? " open" : ""));
    head.dataset.act = "toggle-project";
    head.dataset.proj = key;
    head.innerHTML =
      `<span class="caret">▸</span><span class="pname">${esc(projBase(key))}</span>` +
      `<span class="pcount">${list.length}</span>` +
      `<button class="proj-new" data-act="new-in-project" data-proj="${esc(key)}" ` +
      `title="New session in ${esc(projBase(key))}" aria-label="New session in ${esc(projBase(key))}">+</button>`;
    box.appendChild(head);
    if (open) for (const s of list) {
      box.appendChild(item(s));
      // Show the open session's branches as nested rows (only when it actually
      // has more than one — a linear chat needs no branch list).
      if (s.id === state.sessionId) {
        const branches = treeBranches(state.tree);
        if (branches.length > 1) for (const b of branches) box.appendChild(branchRow(b));
      }
    }
  }
}

export function toggleProject(key) {
  if (!state.openProjects) {
    // Seed from the current default (active project open) so the first click is
    // a deliberate add/remove rather than a reset.
    state.openProjects = new Set();
    const activeProj = state.tree ? state.tree.project : null;
    if (activeProj) state.openProjects.add(activeProj);
  }
  if (state.openProjects.has(key)) state.openProjects.delete(key);
  else state.openProjects.add(key);
  renderSidebar();
}

// ---- rendering: right pane ----------------------------------------------

// Tab badges only — cheap, no body wipe, so the poller can keep counts live
// without clobbering whatever the user is clicking in the tab body.
export function renderTabCounts() {
  const run = state.viewChildId ? state.childRun : state.run;
  const diffN = state.diff && state.tree && state.diff.sessionId === state.tree.id
    ? state.diff.files.length : 0;
  const counts = {
    tree: null,
    changes: diffN,
    network: run && run.network ? run.network.length : 0,
    caps: (state.tree && state.tree.groups ? state.tree.groups.length : 0),
    // The pane is about *live* work — badge the running count, not the registry.
    subagents: state.subagents.filter((s) => s.status === "running").length,
  };
  const labels = { tree: "Tree", changes: "Changes", network: "Network", caps: "Capabilities", subagents: "Subagents" };
  document.querySelectorAll("#tabs button").forEach((b) => {
    const t = b.dataset.tab;
    const n = counts[t];
    b.innerHTML = esc(labels[t]) + (n ? ` <span class="tabct">${n}</span>` : "");
    b.classList.toggle("active", t === state.rightTab);
  });
}

export function renderRight() {
  renderTabCounts();
  const body = $("#tabbody");
  body.innerHTML = "";
  if (!state.tree) { body.appendChild(el("div", "hint", "Open or create a session.")); return; }
  if (state.rightTab === "tree") renderTree(body);
  else if (state.rightTab === "changes") renderChanges(body);
  else if (state.rightTab === "network") renderNetwork(body);
  else if (state.rightTab === "caps") renderCaps(body);
  else if (state.rightTab === "subagents") renderSubagents(body);
}

export function renderTree(body) {
  const tb = el("div", "tree-toolbar");
  if (state.graftRoot) {
    tb.innerHTML = `<span class="hint">graft: pick a parent for <b>${esc(clip(nodeLabel(state.graftRoot), 24))}</b></span>
      <button class="ghost" data-act="graft-cancel">cancel</button>`;
  } else {
    tb.innerHTML = `<span class="hint">click a node to open it — fork or graft from there</span>`;
  }
  const mapBtn = el("button", "ghost map-open", "⤢ Map");
  mapBtn.title = "Open the 2-D session map";
  mapBtn.onclick = openMap;
  tb.appendChild(mapBtn);
  body.appendChild(tb);

  // Same collapsed conversation graph and trunk-first ordering the map uses, so
  // a linear chat reads as a straight vertical list (no diagonal staircase) and
  // only real forks indent — the off-branch turns step out under their parent.
  const { vnodes, vchildren } = visibleGraph(state.tree, state.mapShowSuperseded);
  const ordered = branchOrder(vchildren, state.tree);

  const walk = (id, indent, branched) => {
    const obj = vnodes.get(id);
    if (!obj) return;
    const e = obj.entry;
    const isTip = !((vchildren.get(id) || []).length);
    const row = el("div", "node" +
      (id === state.tree.active_leaf ? " leaf" : "") +
      (isTip ? " tip" : "") +
      (id === state.graftRoot ? " graftroot" : "") +
      (e.grafted_from ? " grafted" : "") +
      (branched ? " branch" : ""));
    row.style.marginLeft = indent * 16 + "px";
    row.innerHTML =
      (branched ? `<span class="tconn">└</span>` : "") +
      `<span class="role ${esc(e.role)}">${e.role === "user" ? "you" : "bgh"}</span>` +
      (e.grafted_from ? `<span class="gmark" title="grafted">↪</span>` : "") +
      `<span class="snippet">${esc(clip(e.content, 44))}</span>` +
      (isTip ? `<span class="tipdot" title="branch tip">●</span>` : "");
    // Click a node to open it; when arming a graft, click the target parent.
    row.onclick = () => {
      if (state.graftRoot && state.graftRoot !== id) graftOnto(id);
      else inspectNode(e);
    };
    body.appendChild(row);
    // The primary child continues the trunk at the same indent; later siblings
    // are forks that step out one level.
    ordered(id).forEach((k, i) => walk(k, i === 0 ? indent : indent + 1, i !== 0));
  };
  (vchildren.get(null) || []).forEach((r, i) => walk(r, 0, i !== 0));
}

// ---- session map (pannable / zoomable 2-D tree) --------------------------
// The right-pane Tree tab is a quick outline; the map is the spatial view —
// a tidy-tree layout you pan (drag) and zoom (scroll), with branches splaying
// out so forks and grafts read at a glance. Camera lives in state.mapView so a
// poll-driven refresh never yanks the viewport.

export async function switchBranch(leafId) {
  if (!leafId || !state.tree || leafId === state.tree.active_leaf) return;
  closeNav(); // picked a branch from the sessions overlay — reveal the transcript
  await forkNode(leafId);
}

// Adopt a branch as trunk: restore the project dir to it and move the trunk
// pointer. The one place the working tree changes on purpose.

export async function adoptBranch(leafId) {
  if (!leafId || !state.tree || leafId === state.tree.trunk_leaf) return;
  try {
    state.tree = await api.adopt(state.sessionId, leafId);
    state.run = await api.run(state.sessionId).catch(() => state.run);
    state.diff = null;
    toast("adopted to trunk");
    render();
  } catch (e) { toast(String(e.message || e), true); }
}

// Inline-rename a branch: swap its name for an input (Enter saves via the label
// endpoint, Esc cancels). Matches the app's no-native-prompt style.

export function renameBranch(row, b) {
  const span = row.querySelector(".bname");
  const input = el("input", "bname-edit");
  input.value = b.named ? b.name : "";
  input.placeholder = "name this branch";
  span.replaceWith(input);
  input.focus(); input.select();
  let done = false;
  const finish = async (commit) => {
    if (done) return; done = true;
    if (commit) {
      try { state.tree = await api.label(state.sessionId, b.leafId, input.value); }
      catch (e) { toast(String(e.message || e), true); }
    }
    render();
  };
  input.onkeydown = (e) => {
    if (e.key === "Enter") { e.preventDefault(); finish(true); }
    else if (e.key === "Escape") { e.preventDefault(); finish(false); }
  };
  input.onblur = () => finish(true);
  input.onclick = (e) => e.stopPropagation();
}

export async function refreshDiff() {
  if (!state.sessionId) return;
  const id = state.sessionId;
  try {
    const d = await api.diff(id);
    state.diff = { sessionId: id, ...d };
  } catch {
    state.diff = { sessionId: id, git: false, files: [], patch: "" };
  }
  if (state.rightTab === "changes") renderRight();
}

export function renderChanges(body) {
  const fresh = state.diff && state.diff.sessionId === state.tree.id;
  if (!fresh) {
    body.appendChild(el("div", "hint", "Loading changes…"));
    refreshDiff();
    return;
  }
  const d = state.diff;

  const head = el("div", "changes-head");
  head.innerHTML =
    `<span class="caps-sub">Working changes</span>` +
    `<button class="mini ghost" data-act="diff-refresh" title="Re-read the workspace">↻</button>`;
  body.appendChild(head);

  if (!d.git) {
    body.appendChild(el("div", "hint",
      "Not a git repo — no diff to show. Changes review reads the workspace's uncommitted git changes."));
    return;
  }
  if (d.files.length === 0) {
    body.appendChild(el("div", "hint", "No uncommitted changes — the workspace matches its last commit."));
    return;
  }

  const sum = el("div", "changes-sum");
  sum.innerHTML = `<b>${d.files.length}</b> file${d.files.length === 1 ? "" : "s"} · ` +
    `<span class="add">+${countDiffLines(d.patch, "+")}</span> ` +
    `<span class="del">−${countDiffLines(d.patch, "-")}</span>`;
  body.appendChild(sum);

  const flist = el("div", "changes-files");
  for (const f of d.files) {
    const sc = ({ "?": "A", A: "A", M: "M", D: "D", R: "R" })[f.status] || "M";
    const row = el("div", "cfile");
    row.innerHTML = `<span class="cst s-${sc}">${esc(f.status || "?")}</span>` +
      `<span class="cpath" title="${esc(f.path)}">${esc(f.path)}</span>`;
    flist.appendChild(row);
  }
  body.appendChild(flist);
  body.appendChild(renderDiff(d.patch));
}

export function countDiffLines(patch, sign) {
  let n = 0;
  for (const l of (patch || "").split("\n")) {
    if (l[0] === sign && !l.startsWith(sign.repeat(3))) n++;
  }
  return n;
}

// A unified diff as colored, line-per-block spans (adds green, dels red,
// hunks amber, file headers dim).

export function renderDiff(patch) {
  const pre = el("pre", "diff");
  let html = "";
  for (const line of (patch || "").split("\n")) {
    let cls = "dl";
    if (/^(diff --git|index |--- |\+\+\+ |new file|deleted file|rename )/.test(line)) cls = "dl dmeta";
    else if (line.startsWith("@@")) cls = "dl dhunk";
    else if (line.startsWith("+")) cls = "dl dadd";
    else if (line.startsWith("-")) cls = "dl ddel";
    html += `<span class="${cls}">${esc(line) || "&nbsp;"}</span>`;
  }
  pre.innerHTML = html;
  return pre;
}

export function renderNetwork(body) {
  const net = state.run && state.run.network ? state.run.network : [];
  const leashed = !!(state.config && state.config.net);

  const posture = el("div", "net-posture " + (leashed ? "leashed" : "blocked"));
  posture.innerHTML = leashed
    ? `<span class="dot">◉</span><div><b>Leashed</b> — default-deny allowlist; a denied request pauses for your approval.</div>`
    : `<span class="dot">⦸</span><div><b>Blocked</b> — sandboxed commands have no network. Start with <code>BOUGH_NET=1</code> to leash instead.</div>`;
  body.appendChild(posture);

  if (net.length === 0) {
    body.appendChild(el("div", "hint", leashed
      ? "No requests itemized yet. Egress the engine observes appears here; code-mode bash is policy-enforced but isn't streamed (the mitmproxy flushes its audit when the run finalizes)."
      : "Nothing to itemize while the network is off."));
    return;
  }
  for (const ev of net) {
    const row = el("div", "net-row " + ev.decision);
    const mp = ev.method ? `${ev.method} ${ev.path || ""}` : "";
    row.innerHTML = `<span class="dot">${ev.decision === "allow" ? "✓" : "✗"}</span>` +
      `<span class="host">${esc(ev.host)}</span>` +
      `<span class="pathm">${esc(mp)}</span>`;
    row.onclick = () => inspectNet(ev);
    body.appendChild(row);
  }
}

// Packs: saved bundles of capability groups + network allow-rules, applied to a
// session up front. Sits atop the Capabilities panel.

export function renderPacks(body) {
  const head = el("div", "packs-head");
  head.appendChild(el("span", "caps-sub", "Packs"));
  const acts = el("div", "packs-actions");
  acts.innerHTML =
    `<button class="mini ghost" data-act="pack-draft">✦ Draft with AI</button>` +
    `<button class="mini ghost" data-act="pack-save-current">Save current</button>`;
  head.appendChild(acts);
  body.appendChild(head);

  if (state.packs.length === 0) {
    body.appendChild(el("div", "hint",
      "No packs yet. Draft one with AI, or save this session's enabled groups + allowlist as a reusable pack."));
    return;
  }
  for (const p of state.packs) {
    const row = el("div", "pack");
    const counts = `${p.allow.length} host${p.allow.length === 1 ? "" : "s"} · ${p.groups.length} group${p.groups.length === 1 ? "" : "s"}`;
    const meta = el("div", "pack-meta");
    meta.innerHTML = `<b>${esc(p.name)}</b>` +
      (p.description ? `<div class="pdesc">${esc(clip(p.description, 70))}</div>` : "") +
      `<div class="pcounts">${counts}</div>`;
    meta.onclick = () => inspectPack(p);
    row.appendChild(meta);
    const apply = el("button", "mini primary", "Apply");
    apply.dataset.act = "pack-apply"; apply.dataset.name = p.name;
    row.appendChild(apply);
    const del = el("button", "mini x", "✕");
    del.dataset.act = "pack-delete"; del.dataset.name = p.name;
    del.title = "Delete pack";
    row.appendChild(del);
    body.appendChild(row);
  }
}

export async function refreshPacks() {
  try { state.packs = await api.packs(); } catch { state.packs = []; }
  if (state.rightTab === "caps") renderRight();
}

export async function applyPack(name) {
  if (!state.sessionId) { toast("Open a session first.", true); return; }
  try {
    state.tree = await api.applyPacks(state.sessionId, [name]);
    toast(`Applied “${name}”`);
    render();
  } catch (e) { toast(String(e.message || e), true); }
}

export async function deletePackByName(name) {
  try { await api.deletePack(name); await refreshPacks(); toast("Pack deleted"); }
  catch (e) { toast(String(e.message || e), true); }
}

export function inspectPack(p) {
  const body = el("div");
  if (p.description) {
    const f = el("div", "field");
    f.appendChild(el("div", "flabel", "about"));
    f.appendChild(el("div", "fval", esc(p.description)));
    body.appendChild(f);
  }
  body.appendChild(listField(`network allowlist (${p.allow.length})`, p.allow));
  body.appendChild(listField(`capability groups (${p.groups.length})`, p.groups));
  const acts = el("div", "drawer-actions");
  const apply = el("button", "primary", "Apply to session");
  apply.onclick = () => { applyPack(p.name); closeDrawer(); };
  acts.appendChild(apply);
  openDrawer("pack", p.name, body, acts);
}

export function listField(label, items) {
  const f = el("div", "field");
  f.appendChild(el("div", "flabel", label));
  if (items.length === 0) f.appendChild(el("div", "fval", "—"));
  else for (const it of items) {
    const row = el("div", "path-row");
    row.innerHTML = `<span class="pp">${esc(it)}</span>`;
    f.appendChild(row);
  }
  return f;
}

// "Save current" — capture the session's enabled groups + allowlist as a pack.

export function savePackCurrent() {
  if (!state.tree) { toast("Open a session first.", true); return; }
  const groups = state.tree.groups || [];
  const allow = state.tree.allow_domains || [];
  if (groups.length === 0 && allow.length === 0) {
    toast("Nothing to save yet — no groups or allow-rules enabled in this session.", true);
    return;
  }
  const body = el("div");
  const nameF = el("div", "field");
  nameF.appendChild(el("div", "flabel", "pack name"));
  const nameInput = el("input", "fin"); nameInput.type = "text"; nameInput.placeholder = "e.g. node-github";
  nameF.appendChild(nameInput);
  body.appendChild(nameF);
  const descF = el("div", "field");
  descF.appendChild(el("div", "flabel", "description"));
  const descInput = el("input", "fin"); descInput.type = "text"; descInput.placeholder = "what this pack is for";
  descF.appendChild(descInput);
  body.appendChild(descF);
  body.appendChild(listField(`network allowlist (${allow.length})`, allow));
  body.appendChild(listField(`capability groups (${groups.length})`, groups));

  const save = async () => {
    const name = nameInput.value.trim();
    if (!name) { toast("Enter a pack name.", true); nameInput.focus(); return; }
    try {
      await api.savePack({ name, description: descInput.value.trim(), groups, allow });
      closeDrawer(); await refreshPacks(); toast(`Saved “${name}”`);
    } catch (e) { toast(String(e.message || e), true); }
  };
  nameInput.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); save(); } };
  const acts = el("div", "drawer-actions");
  const btn = el("button", "primary", "Save pack"); btn.onclick = save;
  acts.appendChild(btn);
  openDrawer("pack", "Save current as pack", body, acts);
  setTimeout(() => nameInput.focus(), 60);
}

// "Draft with AI" — describe the work, get a draft pack to review, edit, save.

export function draftPackFlow() {
  const body = el("div");
  body.appendChild(el("div", "hint",
    "Describe the work and bough drafts a least-privilege allowlist (hosts + capability groups) for you to review before saving."));
  const f = el("div", "field");
  f.appendChild(el("div", "flabel", "what are you doing?"));
  const ta = el("textarea", "fin"); ta.rows = 3;
  ta.placeholder = "e.g. A Python project that installs from PyPI and calls the OpenAI API.";
  f.appendChild(ta);
  body.appendChild(f);
  const acts = el("div", "drawer-actions");
  const btn = el("button", "primary", "✦ Draft");
  btn.onclick = async () => {
    const desc = ta.value.trim();
    if (!desc) { toast("Describe the work first.", true); ta.focus(); return; }
    btn.disabled = true; btn.textContent = "Drafting…";
    try {
      const draft = await api.draftPack(desc);
      if (state.groupsCatalog.length === 0)
        state.groupsCatalog = await api.groupsCatalog().catch(() => []);
      packReview(desc, draft);
    } catch (e) { toast(String(e.message || e), true); btn.disabled = false; btn.textContent = "✦ Draft"; }
  };
  acts.appendChild(btn);
  openDrawer("pack", "Draft a pack", body, acts);
  setTimeout(() => ta.focus(), 60);
}

// Editable review of a drafted pack before saving.

export function packReview(description, draft) {
  const body = el("div");
  const nameF = el("div", "field");
  nameF.appendChild(el("div", "flabel", "pack name"));
  const nameInput = el("input", "fin"); nameInput.type = "text"; nameInput.placeholder = "name this pack";
  nameF.appendChild(nameInput);
  body.appendChild(nameF);

  const allowF = el("div", "field");
  allowF.appendChild(el("div", "flabel", "network allowlist — one per line"));
  const allowTa = el("textarea", "fin"); allowTa.rows = Math.max(3, (draft.allow || []).length + 1);
  allowTa.value = (draft.allow || []).join("\n");
  allowF.appendChild(allowTa);
  body.appendChild(allowF);

  const groupsF = el("div", "field");
  groupsF.appendChild(el("div", "flabel", "capability groups"));
  const chosen = new Set(draft.groups || []);
  const toggleable = state.groupsCatalog.filter((g) => !g.locked);
  if (toggleable.length === 0) groupsF.appendChild(el("div", "fval", "—"));
  for (const g of toggleable) {
    const item = el("label", "gate-group");
    const cb = el("input"); cb.type = "checkbox"; cb.checked = chosen.has(g.name); cb.dataset.pgroup = g.name;
    item.appendChild(cb);
    const meta = el("div", "gg-meta");
    meta.innerHTML = `<b>${esc(g.name)}</b><div class="gg-desc">${esc(clip(g.description, 70))}</div>`;
    item.appendChild(meta);
    groupsF.appendChild(item);
  }
  body.appendChild(groupsF);

  const save = async () => {
    const name = nameInput.value.trim();
    if (!name) { toast("Name the pack.", true); nameInput.focus(); return; }
    const allow = allowTa.value.split("\n").map((s) => s.trim()).filter(Boolean);
    const groups = [...body.querySelectorAll("input[type=checkbox][data-pgroup]")].filter((c) => c.checked).map((c) => c.dataset.pgroup);
    try {
      await api.savePack({ name, description, groups, allow });
      closeDrawer(); await refreshPacks(); toast(`Saved “${name}”`);
    } catch (e) { toast(String(e.message || e), true); }
  };
  const acts = el("div", "drawer-actions");
  const btn = el("button", "primary", "Save pack"); btn.onclick = save;
  acts.appendChild(btn);
  openDrawer("pack", "Review draft", body, acts);
  setTimeout(() => nameInput.focus(), 60);
}

export function renderCaps(body) {
  renderPacks(body);
  body.appendChild(el("div", "caps-sub", "Capability groups"));
  if (state.groupsCatalog.length === 0) {
    body.appendChild(el("div", "hint", "No capability groups for this host."));
    return;
  }

  // Search box over group name + description.
  const search = el("input", "search caps-search");
  search.id = "caps-search";
  search.type = "search";
  search.placeholder = "Filter capabilities…";
  search.setAttribute("aria-label", "Filter capabilities");
  search.value = state.capsFilter;
  search.oninput = (e) => {
    state.capsFilter = e.target.value;
    renderRight();
    const n = $("#caps-search");
    if (n) { n.focus(); const v = n.value; n.setSelectionRange(v.length, v.length); }
  };
  body.appendChild(search);

  const q = state.capsFilter.trim().toLowerCase();
  const matches = (g) => !q ||
    g.name.toLowerCase().includes(q) ||
    (g.description || "").toLowerCase().includes(q);

  const enabled = new Set(state.tree.groups || []);
  const suggested = new Set(state.tree.suggested || []);

  // Split the catalog into subsections by status. "Always on" (locked) is the
  // least actionable, so it starts collapsed; the toggleable sections open.
  const sections = [
    { key: "suggested", title: "Suggested", defaultOpen: true,
      groups: state.groupsCatalog.filter((g) => !g.locked && suggested.has(g.name) && !enabled.has(g.name)) },
    { key: "available", title: "Available", defaultOpen: true,
      groups: state.groupsCatalog.filter((g) => !g.locked && !(suggested.has(g.name) && !enabled.has(g.name))) },
    { key: "alwayson", title: "Always on", defaultOpen: false,
      groups: state.groupsCatalog.filter((g) => g.locked) },
  ];
  for (const s of sections) s.groups = s.groups.filter(matches);
  if (q && sections.every((s) => s.groups.length === 0)) {
    body.appendChild(el("div", "hint", "No capabilities match your search."));
    return;
  }
  // A search makes collapse state moot — show every matching section open.
  for (const s of sections) capsSection(body, s, enabled, suggested, !!q);
}

// One collapsible subsection of the Capabilities pane.

export function capsSection(body, sec, enabled, suggested, forceOpen) {
  if (sec.groups.length === 0) return;
  const open = forceOpen || (sec.key in state.capsOpen ? state.capsOpen[sec.key] : sec.defaultOpen);
  const head = el("div", "caps-group-head" + (open ? " open" : ""));
  head.dataset.act = "toggle-caps-section";
  head.dataset.key = sec.key;
  head.innerHTML = `<span class="caret">▸</span><span>${esc(sec.title)}</span>` +
    `<span class="tabct">${sec.groups.length}</span>`;
  body.appendChild(head);
  if (!open) return;
  for (const g of sec.groups) {
    const row = el("div", "group");
    if (g.locked) {
      row.appendChild(el("span", "cbspace", ""));
    } else {
      const cb = el("input", "");
      cb.type = "checkbox";
      cb.checked = enabled.has(g.name);
      cb.dataset.act = "toggle-group";
      cb.dataset.name = g.name;
      row.appendChild(cb);
    }
    const name = el("div", "gname");
    const tag = g.locked
      ? `<span class="locked">always on</span>`
      : (suggested.has(g.name) ? `<span class="suggested">suggested</span>` : "");
    name.innerHTML = `<b>${esc(g.name)}</b> ${tag}<div class="gdesc">${esc(clip(g.description, 84))}</div>`;
    name.onclick = (e) => { e.stopPropagation(); inspectGroup(g.name); };
    row.appendChild(name);
    // Clicking the row (anywhere but the name, which inspects) toggles the group —
    // a bigger, less fiddly hit target than the checkbox alone.
    if (!g.locked) {
      row.style.cursor = "pointer";
      // Skip the checkbox itself (it has its own toggle handler) to avoid a
      // double-toggle; the name stops propagation above.
      row.onclick = (e) => { if (e.target.tagName !== "INPUT") toggleGroup(g.name, !enabled.has(g.name)); };
    }
    body.appendChild(row);
  }
}

export function renderSubagents(body) {
  if (state.subagents.length === 0) {
    body.appendChild(el("div", "hint",
      "No subagents. The supervisor spawns these for self-contained sub-tasks — they appear here while running, and you can open one to follow its progress or message it."));
    return;
  }
  const running = state.subagents.filter((s) => s.status === "running");
  const finished = state.subagents.filter((s) => s.status !== "running");

  // Live subagents are the point of this pane.
  body.appendChild(el("div", "caps-sub", "Running"));
  if (running.length === 0) {
    body.appendChild(el("div", "hint", "Nothing running right now."));
  } else {
    for (const s of running) body.appendChild(subRow(s, true));
  }

  // Completed ones are collapsed below so they don't crowd out live work.
  if (finished.length) {
    const head = el("div", "sub-done-head" + (state.showDoneSubs ? " open" : ""));
    head.dataset.act = "toggle-done-subs";
    head.innerHTML = `<span class="caret">▸</span><span>Completed</span>` +
      `<span class="tabct">${finished.length}</span>`;
    body.appendChild(head);
    if (state.showDoneSubs) for (const s of finished) body.appendChild(subRow(s, false));
  }
}

export function subRow(s, live) {
  const row = el("div", "sub" + (live ? " live" : ""));
  row.dataset.act = "open-child";
  row.dataset.id = s.id;
  row.innerHTML =
    (live ? `<span class="spin"><span class="pulse"></span></span>` : "") +
    `<span class="stitle">${esc(s.title)}</span>` +
    `<span class="st ${esc(s.status)}">${esc(s.status)}</span>` +
    `<span class="sgo">›</span>`;
  return row;
}

// ---- top-level render ----------------------------------------------------
