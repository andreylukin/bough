"use strict";

// bough web client. A thin SPA over the headless server's JSON API
// (see packages/bough_tui/src/bough_tui/client.gleam for the contract).
// No build step: vanilla JS, fetch + polling.

const ACTIVE = new Set(["running", "awaiting_plan", "awaiting_net", "awaiting_group"]);

const state = {
  config: { provider: "", model: "" },
  sessions: [],
  sessionId: null,
  tree: null,            // { id, project, active_leaf, entries[], superseded[], groups[], suggested[] }
  run: null,             // { status, steps[], text, context_tokens, network[] }
  subagents: [],
  groupsCatalog: [],
  rightTab: "tree",
  reviewArmed: false,
  graftRoot: null,       // node id selected as a graft section root
  viewChildId: null,     // when set, the transcript shows this subagent
  childTree: null,
  childRun: null,
  poll: null,
  filter: "",            // sidebar search query
  lastFocus: null,       // focus to restore when the drawer closes
  lastSig: null,         // last rendered run signature (skip no-op re-renders)
};

// ---- API -----------------------------------------------------------------

async function jget(path) {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`GET ${path} → ${r.status}`);
  return r.json();
}
async function jpost(path, body) {
  const r = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body || {}),
  });
  if (!r.ok) throw new Error(`POST ${path} → ${r.status}: ${await r.text()}`);
  const t = await r.text();
  return t ? JSON.parse(t) : {};
}

const api = {
  config: () => jget("/config"),
  sessions: () => jget("/sessions"),
  createSession: (project) => jpost("/session", { project }),
  tree: (id) => jget(`/session/${id}`),
  run: (id) => jget(`/session/${id}/run`),
  startRun: (id, content, review) => jpost(`/session/${id}/run`, { content, review }),
  control: (id, decision, message) => jpost(`/session/${id}/control`, { decision, message: message || "" }),
  fork: (id, entry_id) => jpost(`/session/${id}/fork`, { entry_id }),
  graft: (id, section_root, onto) => jpost(`/session/${id}/graft`, { section_root, onto }),
  subagents: (id) => jget(`/session/${id}/subagents`),
  groupsCatalog: () => jget("/groups"),
  groupDetail: (name) => jget(`/groups/${name}`),
  setGroups: (id, groups) => jpost(`/session/${id}/groups`, { groups }),
};

// ---- helpers -------------------------------------------------------------

const $ = (sel) => document.querySelector(sel);
const el = (tag, cls, html) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (html != null) e.innerHTML = html;
  return e;
};
const esc = (s) => (s || "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const clip = (s, n) => { s = (s || "").replace(/\s+/g, " ").trim(); return s.length > n ? s.slice(0, n) + "…" : s; };
const ago = (ms) => {
  if (!ms) return "";
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return s + "s ago";
  const m = Math.floor(s / 60); if (m < 60) return m + "m ago";
  const h = Math.floor(m / 60); if (h < 24) return h + "h ago";
  const d = Math.floor(h / 24); return d + "d ago";
};

// ---- tiny markdown -> HTML (self-contained, no deps) ---------------------

function mdInline(escaped) {
  return escaped
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*])\*([^*\n]+)\*/g, "$1<em>$2</em>")
    .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
}

function md(src) {
  const lines = (src || "").split("\n");
  let html = "", i = 0, list = null;
  const closeList = () => { if (list) { html += `</${list}>`; list = null; } };
  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(/^```(\w*)\s*$/);
    if (fence) {
      closeList(); i++;
      const code = [];
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { code.push(lines[i]); i++; }
      i++;
      html += `<pre class="code"><code>${esc(code.join("\n"))}</code></pre>`;
      continue;
    }
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) { closeList(); const l = Math.min(h[1].length + 2, 6); html += `<h${l} class="mdh">${mdInline(esc(h[2]))}</h${l}>`; i++; continue; }
    if (/^\s{0,3}>\s?/.test(line)) { closeList(); html += `<blockquote>${mdInline(esc(line.replace(/^\s{0,3}>\s?/, "")))}</blockquote>`; i++; continue; }
    if (/^\s*[-*+]\s+/.test(line)) {
      if (list !== "ul") { closeList(); html += "<ul>"; list = "ul"; }
      html += `<li>${mdInline(esc(line.replace(/^\s*[-*+]\s+/, "")))}</li>`; i++; continue;
    }
    if (/^\s*\d+\.\s+/.test(line)) {
      if (list !== "ol") { closeList(); html += "<ol>"; list = "ol"; }
      html += `<li>${mdInline(esc(line.replace(/^\s*\d+\.\s+/, "")))}</li>`; i++; continue;
    }
    if (line.trim() === "") { closeList(); i++; continue; }
    closeList();
    const para = [line]; i++;
    while (i < lines.length && lines[i].trim() !== "" &&
      !/^(```|#{1,6}\s|\s{0,3}>\s?|\s*[-*+]\s+|\s*\d+\.\s+)/.test(lines[i])) { para.push(lines[i]); i++; }
    html += `<p>${mdInline(esc(para.join("\n"))).replace(/\n/g, "<br>")}</p>`;
  }
  closeList();
  return html;
}

function copyText(text, okMsg) {
  const done = () => toast(okMsg || "Copied");
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(done).catch(() => fallbackCopy(text, done));
  } else {
    fallbackCopy(text, done);
  }
}
function fallbackCopy(text, done) {
  const ta = document.createElement("textarea");
  ta.value = text; ta.style.position = "fixed"; ta.style.opacity = "0";
  document.body.appendChild(ta); ta.select();
  try { document.execCommand("copy"); done(); } catch { toast("Copy failed", true); }
  document.body.removeChild(ta);
}

let toastTimer = null;
function toast(msg, isErr) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = "show" + (isErr ? " err" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.className = ""), isErr ? 4500 : 2200);
}

function activePath(tree) {
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

function stepCard(step) {
  switch (step.type) {
    case "plan":
    case "text":
      return step.text && step.text.trim()
        ? el("div", "card plan prose", md(step.text)) : null;
    case "call": {
      const verb = (step.verb || "").toLowerCase();
      const card = el("div", "card");
      const head = el("div", "head");
      head.appendChild(el("span", "verb " + verb, esc(step.verb)));
      head.appendChild(el("span", "arg", esc(step.arg || "")));
      card.appendChild(head);
      card.onclick = () => inspectStep(step, null);
      return card;
    }
    case "exec": {
      const ok = step.exit === 0;
      const card = el("div", "card " + (ok ? "ok" : "bad"));
      const head = el("div", "head");
      head.appendChild(el("span", "verb " + (step.verb || "").toLowerCase(), esc(step.verb)));
      head.appendChild(el("span", "arg", ""));
      head.appendChild(el("span", "exit " + (ok ? "ok" : "bad"), "exit " + step.exit));
      card.appendChild(head);
      if (step.digest && step.digest.trim())
        card.appendChild(el("pre", "out", esc(step.digest)));
      card.onclick = () => inspectStep(null, step);
      return card;
    }
    case "worker": {
      const card = el("div", "card");
      const head = el("div", "head");
      head.appendChild(el("span", "tag worker", "worker fix"));
      head.appendChild(el("span", "arg", esc(step.command || "")));
      head.appendChild(el("span", "exit " + (step.exit === 0 ? "ok" : "bad"), "exit " + step.exit));
      card.appendChild(head);
      return card;
    }
    case "check": {
      // A status marker, not a pane: a small check/cross chip. The check's
      // output (if any) is available on hover.
      const c = el("div", "chip " + (step.ok ? "ok" : "bad"),
        `<span class="ic">${step.ok ? "✓" : "✗"}</span> check`);
      if (step.digest && step.digest.trim()) c.title = step.digest;
      return c;
    }
    case "review": {
      // The adversarial-review marker: a small raised-hand chip; the note (often
      // just "requested"/"accepted") sits beside it.
      const note = step.note && step.note.trim() ? ` <span class="note">${esc(step.note)}</span>` : "";
      return el("div", "chip review", `<span class="ic">✋</span> review${note}`);
    }
    case "tool": {
      const card = el("div", "card");
      const head = el("div", "head");
      head.appendChild(el("span", "verb run", esc(step.name)));
      head.appendChild(el("span", "arg", esc(clip(step.input, 120))));
      card.appendChild(head);
      return card;
    }
    case "result":
      return step.output && step.output.trim()
        ? el("div", "card", `<pre>${esc(step.output)}</pre>`) : null;
    // await / net / group are rendered as the gate bar, not inline cards.
    default:
      return null;
  }
}

function gateBar(run, sessionId) {
  const tail = run.steps[run.steps.length - 1];
  if (!tail) return null;
  const bar = el("div", "gate");
  if (run.status === "awaiting_plan" && tail.type === "await") {
    bar.appendChild(el("h4", null, "⏸ Plan review — approve before it runs"));
    bar.appendChild(el("pre", null, esc(tail.plan)));
    const row = el("div", "row");
    row.innerHTML =
      `<button class="accept" data-act="allow">Approve &amp; run</button>` +
      `<input type="text" data-role="steer" placeholder="steer / reason to revise…" />` +
      `<button class="ghost" data-act="steer">Send guidance</button>` +
      `<button class="reject" data-act="reject-plan">Reject</button>`;
    bar.appendChild(row);
  } else if (run.status === "awaiting_net" && tail.type === "net") {
    bar.appendChild(el("h4", null, "🔌 Network — a sandboxed request was denied"));
    bar.appendChild(el("pre", null, esc(tail.detail)));
    const row = el("div", "row");
    row.innerHTML =
      `<button class="accept" data-act="allow">Allow host</button>` +
      `<input type="text" data-role="steer" value="${esc(tail.rule || "")}" placeholder="path-glob rule…" />` +
      `<button class="ghost" data-act="steer">Allow rule</button>` +
      `<button class="reject" data-act="reject">Deny</button>`;
    bar.appendChild(row);
  } else if (run.status === "awaiting_group" && tail.type === "group") {
    bar.appendChild(el("h4", null, "🔑 Capability — a step needs filesystem access"));
    bar.appendChild(el("pre", null, esc(tail.detail)));
    const row = el("div", "row");
    row.innerHTML =
      `<button class="accept" data-act="allow">Enable all</button>` +
      `<input type="text" data-role="steer" value="${esc(tail.groups || "")}" placeholder="group name(s)…" />` +
      `<button class="ghost" data-act="steer">Enable selected</button>` +
      `<button class="reject" data-act="reject">Reject</button>`;
    bar.appendChild(row);
  } else {
    return null;
  }
  return bar;
}

function renderTranscript() {
  const box = $("#transcript");
  box.innerHTML = "";

  // Subagent view: a back bar + the child's transcript.
  if (state.viewChildId) {
    const bar = el("div", "subbar");
    bar.innerHTML = `<button class="ghost" data-act="back-parent">‹ back to parent</button>
      <span>viewing subagent ${esc(state.viewChildId)}</span>`;
    $("#center").insertBefore(bar, box);
    renderConversation(box, state.childTree, state.childRun);
    cleanupSubbar(bar);
    return;
  }
  renderConversation(box, state.tree, state.run);
}

let _subbar = null;
function cleanupSubbar(bar) {
  if (_subbar && _subbar !== bar) _subbar.remove();
  _subbar = bar;
}
function dropSubbar() { if (_subbar) { _subbar.remove(); _subbar = null; } }

function renderConversation(box, tree, run) {
  const path = activePath(tree);
  if (path.length === 0 && (!run || run.status === "idle")) {
    box.appendChild(el("div", "empty",
      "This branch is bare. <strong>Ask bough to do something</strong> and it grows: a frontier model plans, a sandbox runs the work, and a deterministic check decides when it's done — every turn snapshotted so you can fork the history and the files together."));
    return;
  }
  // The run grows along a stem: render contiguous step entries together (so a
  // call pairs with its exec), with user/assistant turns between them.
  const stream = el("div", "stream");
  box.appendChild(stream);
  let steps = [];
  const flush = () => { if (steps.length) { renderStepList(stream, steps); steps = []; } };
  for (const e of path) {
    if (e.role === "tool_result") {
      try { steps.push(JSON.parse(e.content)); } catch {}
    } else if (e.role === "user") {
      flush();
      stream.appendChild(el("div", "msg user", esc(e.content)));
    } else if (e.role === "assistant") {
      // Guard against an older bug where the final reply was also stored as a
      // trailing plan step: drop a plan/text step identical to the answer so it
      // isn't shown twice.
      const ans = (e.content || "").trim();
      steps = steps.filter((s) => !((s.type === "plan" || s.type === "text") && (s.text || "").trim() === ans));
      flush();
      stream.appendChild(el("div", "msg assistant prose", md(e.content)));
    }
    // system digests are hidden and do NOT flush: a digest always sits right
    // before the assistant leaf, and flushing here would render the trailing
    // plan step before the dedupe above can drop it.
  }
  flush();

  const live = run && ACTIVE.has(run.status);
  if (live) {
    renderStepList(stream, run.steps);
    const gate = gateBar(run, tree ? tree.id : state.sessionId);
    if (gate) stream.appendChild(gate);
    else stream.appendChild(el("div", "card plan",
      `<span class="spin"><span class="pulse"></span> ${esc(growthLabel(run.status))}</span>`));
  } else if (run && run.status === "error") {
    const c = el("div", "card bad");
    c.innerHTML = `<div class="head"><span class="verb">error</span></div>`;
    c.appendChild(el("pre", "out", esc(run.text)));
    stream.appendChild(c);
  }
  box.scrollTop = box.scrollHeight;
}

function growthLabel(status) {
  return { running: "growing…", awaiting_plan: "waiting for you", awaiting_net: "waiting for you", awaiting_group: "waiting for you" }[status] || status + "…";
}

// ---- inspector drawer ----------------------------------------------------
// Any element you click opens this with everything known about it.

function openDrawer(kind, title, bodyEl, actionsEl) {
  const d = $("#drawer");
  state.lastFocus = document.activeElement;
  d.innerHTML = "";
  const head = el("div", "drawer-head");
  head.innerHTML = `<span class="kind">${esc(kind)}</span><h3>${esc(title)}</h3>`;
  const x = el("button", "x", "✕"); x.setAttribute("aria-label", "Close"); x.onclick = closeDrawer; head.appendChild(x);
  d.appendChild(head);
  const body = el("div", "drawer-body"); body.appendChild(bodyEl); d.appendChild(body);
  if (actionsEl) d.appendChild(actionsEl);
  d.classList.add("open"); d.setAttribute("aria-hidden", "false");
  $("#scrim").classList.add("show");
  x.focus();
}
function closeDrawer() {
  const d = $("#drawer");
  if (!d.classList.contains("open")) return;
  d.classList.remove("open"); d.setAttribute("aria-hidden", "true");
  $("#scrim").classList.remove("show");
  if (state.lastFocus && state.lastFocus.focus) state.lastFocus.focus();
}
function kvField(label, rows) {
  const f = el("div", "field");
  f.appendChild(el("div", "flabel", label));
  for (const [k, v, cls] of rows) {
    if (v == null || v === "") continue;
    const row = el("div", "kv");
    row.innerHTML = `<span class="k">${esc(k)}</span><span class="v ${cls || ""}">${esc(v)}</span>`;
    f.appendChild(row);
  }
  return f;
}
function preField(label, text) {
  const f = el("div", "field");
  f.appendChild(el("div", "flabel", label));
  f.appendChild(el("pre", null, esc(text)));
  return f;
}

function inspectStep(call, exec) {
  const verb = ((call && call.verb) || (exec && exec.verb) || "step");
  const body = el("div");
  body.appendChild(kvField("details", [
    ["action", verb],
    ["arg", call && call.arg],
    ["exit", exec && String(exec.exit)],
  ]));
  if (call && call.detail && call.detail.trim())
    body.appendChild(preField(verb.toLowerCase() === "code" ? "program" : "content", call.detail));
  if (exec && exec.digest && exec.digest.trim())
    body.appendChild(preField("output", exec.digest));
  openDrawer("step", verb.toLowerCase() === "code" ? "code-mode step" : verb + " step", body);
}

function inspectNode(node) {
  const body = el("div");
  body.appendChild(kvField("node", [
    ["role", node.role],
    ["id", node.id],
    ["grafted", node.grafted_from],
  ]));
  const content = node.role === "tool_result" ? stepLabel(node.content) : node.content;
  body.appendChild(preField("content", content));
  const acts = el("div", "drawer-actions");
  const fork = el("button", "primary", "Fork from here");
  fork.onclick = () => { forkNode(node.id); closeDrawer(); };
  acts.appendChild(fork);
  const graft = el("button", "ghost", "Graft this subtree");
  graft.onclick = () => { state.graftRoot = node.id; state.rightTab = "tree"; renderRight(); closeDrawer(); toast("pick a parent node to graft onto"); };
  acts.appendChild(graft);
  const title = node.role === "user" ? "your message" : node.role === "assistant" ? "bough's reply" : "step";
  openDrawer("history", title, body, acts);
}

function inspectNet(ev) {
  const body = el("div");
  body.appendChild(kvField("egress request", [
    ["decision", ev.decision, ev.decision],
    ["host", ev.host],
    ["port", ev.port ? String(ev.port) : ""],
    ["method", ev.method || "—"],
    ["path", ev.path || "—"],
    ["reason", ev.reason || "—"],
  ]));
  openDrawer("network", ev.host, body);
}

// Render a flat list of steps, pairing each call with the exec that follows it
// into a single card (so a code-mode round is one block, not two "CODE" cards).
function renderStepList(box, steps) {
  for (let i = 0; i < steps.length; i++) {
    const s = steps[i], next = steps[i + 1];
    if (s.type === "call" && next && next.type === "exec") {
      box.appendChild(mergedCard(s, next)); i++; continue;
    }
    const c = stepCard(s); if (c) box.appendChild(c);
  }
}

// A call+exec pair as one clickable card: verb, arg, exit, and an output
// preview. Click opens the drawer with the full program + full output.
function mergedCard(call, exec) {
  const verb = (call.verb || "").toLowerCase();
  const ok = exec.exit === 0;
  const card = el("div", "card " + (ok ? "ok" : "bad"));
  const head = el("div", "head");
  head.appendChild(el("span", "verb " + verb, esc(call.verb)));
  head.appendChild(el("span", "arg", esc(call.arg || "")));
  head.appendChild(el("span", "exit " + (ok ? "ok" : "bad"), "exit " + exec.exit));
  card.appendChild(head);
  if (exec.digest && exec.digest.trim())
    card.appendChild(el("pre", "out", esc(exec.digest)));
  card.onclick = () => inspectStep(call, exec);
  return card;
}

// ---- rendering: header + sidebar ----------------------------------------

function renderHeader() {
  $("#model").textContent = state.config.provider
    ? `${state.config.provider} · ${state.config.model}` : "";
  const ctx = $("#ctx");
  if (!state.tree) { ctx.innerHTML = "no session"; }
  else {
    const st = state.run ? state.run.status : "idle";
    const cls = ACTIVE.has(st) ? (st === "running" ? "running" : "awaiting") : st;
    const tok = state.run && state.run.context_tokens
      ? ` · ${state.run.context_tokens} tok` : "";
    const full = state.tree.project || "";
    const base = full.split("/").filter(Boolean).pop() || full;
    ctx.innerHTML =
      `<span class="proj" title="${esc(full)}">${esc(base)}</span> ` +
      `<span class="sid" data-act="copy-id" data-id="${esc(state.tree.id)}" ` +
      `title="Click to copy session id">${esc(state.tree.id)}</span> ` +
      `<span class="badge ${cls}">${esc(st)}</span>${tok}`;
  }
  $("#review-toggle").checked = state.reviewArmed;
}

function renderSidebar() {
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
  for (const s of sessions) {
    const item = el("div", "session-item" + (s.id === state.sessionId ? " active" : ""));
    item.dataset.act = "open-session";
    item.dataset.id = s.id;
    const proj = (s.project || "").split("/").filter(Boolean).pop() || s.project;
    item.appendChild(el("div", "title", esc(clip(s.title || "untitled task", 60))));
    item.appendChild(el("div", "meta",
      `<span class="proj">${esc(proj)}</span><span>${s.turns} turn${s.turns === 1 ? "" : "s"} · ${ago(s.updated)}</span>`));
    box.appendChild(item);
  }
}

// ---- rendering: right pane ----------------------------------------------

function renderRight() {
  const run = state.viewChildId ? state.childRun : state.run;
  const counts = {
    tree: null,
    network: run && run.network ? run.network.length : 0,
    caps: (state.tree && state.tree.groups ? state.tree.groups.length : 0),
    subagents: state.subagents.length,
  };
  const labels = { tree: "Tree", network: "Network", caps: "Capabilities", subagents: "Subagents" };
  document.querySelectorAll("#tabs button").forEach((b) => {
    const t = b.dataset.tab;
    const n = counts[t];
    b.innerHTML = esc(labels[t]) + (n ? ` <span class="tabct">${n}</span>` : "");
    b.classList.toggle("active", t === state.rightTab);
  });
  const body = $("#tabbody");
  body.innerHTML = "";
  if (!state.tree) { body.appendChild(el("div", "hint", "Open or create a session.")); return; }
  if (state.rightTab === "tree") renderTree(body);
  else if (state.rightTab === "network") renderNetwork(body);
  else if (state.rightTab === "caps") renderCaps(body);
  else if (state.rightTab === "subagents") renderSubagents(body);
}

function renderTree(body) {
  const tb = el("div", "tree-toolbar");
  if (state.graftRoot) {
    tb.innerHTML = `<span class="hint">graft: pick a parent for <b>${esc(clip(nodeLabel(state.graftRoot), 24))}</b></span>
      <button class="ghost" data-act="graft-cancel">cancel</button>`;
  } else {
    tb.innerHTML = `<span class="hint">click ⑂ to fork a node, or graft a subtree</span>`;
  }
  body.appendChild(tb);

  const sup = new Set(state.tree.superseded || []);
  const byId = new Map(state.tree.entries.map((e) => [e.id, e]));
  const children = new Map();
  for (const e of state.tree.entries) {
    if (!children.has(e.parent_id)) children.set(e.parent_id, []);
    children.get(e.parent_id).push(e);
  }
  const roots = state.tree.entries.filter((e) => !e.parent_id || !byId.has(e.parent_id));

  // The tree is a branching map of the *conversation*: only user prompts and
  // assistant replies are fork points. Intermediate step nodes (tool_result)
  // and digests (system) are passed through so the tree stays legible.
  const walk = (node, depth) => {
    if (sup.has(node.id)) return; // hide superseded by default
    if (node.role === "system" || node.role === "tool_result") {
      (children.get(node.id) || []).forEach((c) => walk(c, depth));
      return;
    }
    const row = el("div", "node" + (node.id === state.tree.active_leaf ? " leaf" : "") +
      (node.id === state.graftRoot ? " graftroot" : ""));
    row.style.marginLeft = depth * 14 + "px";
    row.innerHTML =
      `<span class="role ${esc(node.role)}">${node.role === "user" ? "you" : "bgh"}</span>` +
      `<span class="snippet">${esc(clip(node.content, 46))}</span>`;
    // Click a node to open it: when arming a graft, click the target parent.
    row.onclick = () => {
      if (state.graftRoot && state.graftRoot !== node.id) graftOnto(node.id);
      else inspectNode(node);
    };
    body.appendChild(row);
    (children.get(node.id) || []).forEach((c) => walk(c, depth + 1));
  };
  roots.forEach((r) => walk(r, 0));
}

function nodeLabel(id) {
  const e = state.tree.entries.find((x) => x.id === id);
  return e ? (e.role === "tool_result" ? stepLabel(e.content) : e.content) : id;
}
function stepLabel(content) {
  try {
    const s = JSON.parse(content);
    if (s.type === "call" || s.type === "exec") return `${s.verb} ${s.arg || ""}`;
    if (s.type === "check") return s.ok ? "CHECK ✓" : "CHECK ✗";
    if (s.type === "plan" || s.type === "text") return s.text;
    return s.type;
  } catch { return content; }
}

function renderNetwork(body) {
  const net = state.run && state.run.network ? state.run.network : [];
  if (net.length === 0) {
    body.appendChild(el("div", "hint",
      "No egress observed. The live feed populates under the leash (start the server with BOUGH_NET=1)."));
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

function renderCaps(body) {
  if (state.groupsCatalog.length === 0) {
    body.appendChild(el("div", "hint", "No capability groups for this host."));
    return;
  }
  const enabled = new Set(state.tree.groups || []);
  const suggested = new Set(state.tree.suggested || []);
  for (const g of state.groupsCatalog) {
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
    name.onclick = () => inspectGroup(g.name);
    row.appendChild(name);
    body.appendChild(row);
  }
}

function renderSubagents(body) {
  if (state.subagents.length === 0) {
    body.appendChild(el("div", "hint", "No subagents. The supervisor spawns these for self-contained sub-tasks."));
    return;
  }
  for (const s of state.subagents) {
    const row = el("div", "sub");
    row.dataset.act = "open-child";
    row.dataset.id = s.id;
    row.innerHTML = `<span class="stitle">${esc(s.title)}</span>` +
      `<span class="st ${esc(s.status)}">${esc(s.status)}</span>`;
    body.appendChild(row);
  }
}

// ---- top-level render ----------------------------------------------------

function render() {
  renderHeader();
  renderSidebar();
  renderRight();
  renderTranscript();
  if (!state.viewChildId) dropSubbarIfPresentWithoutChild();
  const composer = $("#composer");
  const steering = !state.viewChildId && state.run && ACTIVE.has(state.run.status);
  composer.classList.toggle("steering", !!steering);
  $("#prompt").placeholder = steering
    ? "Steer this run — type and Enter to inject…"
    : (state.viewChildId ? "Message this subagent…" : "Ask bough to do something…  (Enter to send, Shift+Enter for newline)");
}
function dropSubbarIfPresentWithoutChild() { dropSubbar(); }

// ---- actions -------------------------------------------------------------

async function loadSessions() {
  try { state.sessions = await api.sessions(); } catch { state.sessions = []; }
}

async function openSession(id) {
  stopPoll();
  closeDrawer();
  state.sessionId = id;
  state.viewChildId = null; state.childTree = null; state.childRun = null;
  state.graftRoot = null; state.lastSig = null;
  try {
    state.tree = await api.tree(id);
    state.run = await api.run(id).catch(() => null);
    state.subagents = await api.subagents(id).catch(() => []);
  } catch (e) { toast(String(e.message || e), true); return; }
  render();
  if (state.run && ACTIVE.has(state.run.status)) startPoll();
}

// A clean in-app form (no native prompt) to start a session in a project dir.
function newSession() {
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

async function submitComposer() {
  const ta = $("#prompt");
  const text = ta.value.trim();
  if (!text || !state.sessionId) return;

  // Steering a subagent.
  if (state.viewChildId) {
    try { await api.control(state.viewChildId, "steer", text); ta.value = ""; toast("sent to subagent"); }
    catch (e) { toast(String(e.message || e), true); }
    return;
  }
  // Steering the live run.
  if (state.run && ACTIVE.has(state.run.status)) {
    try { await api.control(state.sessionId, "steer", text); ta.value = ""; toast("steering…"); }
    catch (e) { toast(String(e.message || e), true); }
    return;
  }
  // New run.
  ta.value = "";
  try {
    await api.startRun(state.sessionId, text, state.reviewArmed);
    state.tree = await api.tree(state.sessionId);
    state.run = { status: "running", steps: [], text: "", context_tokens: 0, network: [] };
    state.lastSig = null;
    render();
    startPoll();
  } catch (e) { toast(String(e.message || e), true); }
}

async function gateDecision(decision, message) {
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

async function forkNode(entryId) {
  try {
    state.tree = await api.fork(state.sessionId, entryId);
    state.run = await api.run(state.sessionId).catch(() => state.run);
    state.graftRoot = null;
    toast("forked");
    render();
  } catch (e) { toast(String(e.message || e), true); }
}

async function graftOnto(onto) {
  try {
    state.tree = await api.graft(state.sessionId, state.graftRoot, onto);
    state.graftRoot = null;
    toast("grafted");
    render();
  } catch (e) { toast("graft rejected (cycle or unknown node)", true); }
}

async function toggleGroup(name, on) {
  const cur = new Set(state.tree.groups || []);
  if (on) cur.add(name); else cur.delete(name);
  try {
    await api.setGroups(state.sessionId, [...cur]);
    state.tree = await api.tree(state.sessionId);
    render();
  } catch (e) { toast(String(e.message || e), true); }
}

async function inspectGroup(name) {
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

async function openChild(id) {
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

function backToParent() {
  state.viewChildId = null; state.childTree = null; state.childRun = null;
  dropSubbar();
  openSession(state.sessionId);
}

// ---- polling -------------------------------------------------------------

function startPoll() { stopPoll(); state.poll = setInterval(tick, 600); }
function stopPoll() { if (state.poll) { clearInterval(state.poll); state.poll = null; } }

// A cheap fingerprint of everything the transcript/right pane draws, so a poll
// that returns identical state doesn't rebuild the DOM (which caused flicker).
function runSig(run, subs) {
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

async function tick() {
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
  const done = !ACTIVE.has(run.status);
  if (done) {
    state.tree = await api.tree(id).catch(() => state.tree);
    await loadSessions();
    stopPoll();
  }
  const sig = runSig(run, state.subagents);
  if (done || sig !== state.lastSig) { state.lastSig = sig; render(); }
}

// ---- event wiring --------------------------------------------------------

function wire() {
  // Tab switching.
  $("#tabs").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-tab]");
    if (!b) return;
    state.rightTab = b.dataset.tab;
    if (state.rightTab === "caps" && state.groupsCatalog.length === 0)
      api.groupsCatalog().then((g) => { state.groupsCatalog = g; renderRight(); }).catch(() => {});
    renderRight();
  });

  $("#new-session").addEventListener("click", newSession);
  $("#review-toggle").addEventListener("change", (e) => { state.reviewArmed = e.target.checked; });
  $("#session-search").addEventListener("input", (e) => { state.filter = e.target.value; renderSidebar(); });
  $("#scrim").addEventListener("click", closeDrawer);

  // Composer.
  $("#composer").addEventListener("submit", (e) => { e.preventDefault(); submitComposer(); });
  $("#prompt").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); submitComposer(); }
  });

  // Delegated clicks (sidebar, subagents, graft toolbar, gates). Tree nodes,
  // network rows, steps and groups bind their own handlers (they carry data).
  document.addEventListener("click", (e) => {
    const t = e.target.closest("[data-act]");
    if (!t) return;
    const act = t.dataset.act;
    const id = t.dataset.id;
    const steerInput = () => {
      const bar = t.closest(".gate");
      const inp = bar ? bar.querySelector("[data-role=steer]") : null;
      return inp ? inp.value : "";
    };
    switch (act) {
      case "copy-id": copyText(id, "Session id copied"); break;
      case "open-session": openSession(id); break;
      case "open-child": openChild(id); break;
      case "back-parent": backToParent(); break;
      case "graft-cancel": state.graftRoot = null; renderRight(); break;
      case "allow": gateDecision("allow", ""); break;
      case "steer": gateDecision("steer", steerInput()); break;
      case "reject": gateDecision("steer", ""); break; // empty steer = deny (net/group)
      case "reject-plan": gateDecision("steer", steerInput() || "Reject this plan and revise the approach."); break;
    }
  });

  // Toggle a capability group (checkbox change).
  document.addEventListener("change", (e) => {
    const c = e.target.closest("[data-act=toggle-group]");
    if (c) toggleGroup(c.dataset.name, c.checked);
  });

  // Esc closes the inspector drawer.
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeDrawer();
  });
}

// ---- boot ----------------------------------------------------------------

async function boot() {
  wire();
  try { state.config = await api.config(); } catch {}
  await loadSessions();
  if (state.sessions.length > 0) await openSession(state.sessions[0].id);
  else render();
}

boot();
