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
  showDoneSubs: false,   // Subagents pane: reveal the collapsed completed ones
  capsOpen: {},          // Capabilities pane: per-subsection collapse state (key → bool)
  capsFilter: "",        // Capabilities pane: search query over groups
  diff: null,            // { sessionId, git, files[], patch } — lazy-loaded Changes tab
  files: null,           // { sessionId, list[] } — workspace files for the "@" picker
  pastes: [],            // large clipboard pastes collapsed into chips, expanded on send
  groupsCatalog: [],
  packs: [],
  rightTab: "tree",
  reviewArmed: false,
  graftRoot: null,       // node id selected as a graft section root
  mapOpen: false,        // session map overlay (pannable/zoomable 2-D tree)
  mapView: null,         // map camera { x, y, scale }; null = fit on next paint
  mapShowSuperseded: false,
  mapExpanded: new Set(), // map: turn ids whose tool-call steps are expanded
  viewChildId: null,     // when set, the transcript shows this subagent
  childTree: null,
  childRun: null,
  poll: null,
  filter: "",            // sidebar search query
  openProjects: null,    // sidebar: which project groups are expanded (null = default)
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
async function jdel(path) {
  const r = await fetch(path, { method: "DELETE" });
  if (!r.ok) throw new Error(`DELETE ${path} → ${r.status}`);
  return {};
}

const api = {
  config: () => jget("/config"),
  sessions: () => jget("/sessions"),
  createSession: (project) => jpost("/session", { project }),
  tree: (id) => jget(`/session/${id}`),
  run: (id) => jget(`/session/${id}/run`),
  startRun: (id, content, review) => jpost(`/session/${id}/run`, { content, review }),
  control: (id, decision, message) => jpost(`/session/${id}/control`, { decision, message: message || "" }),
  stop: (id) => jpost(`/session/${id}/control`, { decision: "stop" }),
  fork: (id, entry_id) => jpost(`/session/${id}/fork`, { entry_id }),
  graft: (id, section_root, onto) => jpost(`/session/${id}/graft`, { section_root, onto }),
  label: (id, entry_id, label) => jpost(`/session/${id}/label`, { entry_id, label }),
  adopt: (id, entry_id) => jpost(`/session/${id}/adopt`, { entry_id }),
  subagents: (id) => jget(`/session/${id}/subagents`),
  diff: (id) => jget(`/session/${id}/diff`),
  files: (id) => jget(`/session/${id}/files`),
  groupsCatalog: () => jget("/groups"),
  groupDetail: (name) => jget(`/groups/${name}`),
  setGroups: (id, groups) => jpost(`/session/${id}/groups`, { groups }),
  packs: () => jget("/packs"),
  savePack: (pack) => jpost("/packs", pack),
  deletePack: (name) => jdel(`/packs/${encodeURIComponent(name)}`),
  applyPacks: (id, names) => jpost(`/session/${id}/packs`, { names }),
  draftPack: (description) => jpost("/packs/draft", { description }),
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
// The harness appends `[full output saved: /Users/.../out_N.txt]` to a capped
// digest; the absolute path is internal noise, so show the truncation marker
// without it.
const cleanDigest = (s) => (s || "").replace(/\[full output saved: [^\]]*\]/g, "[output truncated]");
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
      html += `<details><summary>Code block</summary><pre class="code"><code>${esc(code.join("\n"))}</code></pre></details>`;
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
    case "text": {
      const t = step.text || "";
      // A delivered subagent result ("⟵ subagent: … Final output: …") is a big
      // block — render it as a labeled card collapsed to its title by default.
      if (t.trim().startsWith("⟵ subagent:")) return subagentReport(t);
      return t.trim() ? el("div", "card plan prose", md(t)) : null;
    }
    case "plan":
      return step.text && step.text.trim()
        ? el("div", "card plan prose", md(step.text)) : null;
    case "call": {
      const verb = (step.verb || "").toLowerCase();
      // A `collect` is a non-blocking status probe, not workspace work — its
      // result is a one-line status, folded into the compact chip below. Drop
      // the call card so polling never grows into a wall of cards.
      if (verb === "collect") return null;
      const card = el("div", "card");
      const head = el("div", "head");
      head.appendChild(el("span", "verb " + verb, esc(step.verb)));
      head.appendChild(el("span", "arg", esc(step.arg || "")));
      card.appendChild(head);
      card.onclick = () => inspectStep(step, null);
      return card;
    }
    case "exec": {
      // A lone `collect` exec (unpaired) still renders as the quiet chip.
      if ((step.verb || "").toUpperCase() === "COLLECT") return collectChip(step);
      const ok = step.exit === 0;
      const card = el("div", "card " + (ok ? "ok" : "bad"));
      const head = el("div", "head");
      head.appendChild(el("span", "verb " + (step.verb || "").toLowerCase(), esc(step.verb)));
      head.appendChild(el("span", "arg", ""));
      head.appendChild(el("span", "exit " + (ok ? "ok" : "bad"), "exit " + step.exit));
      card.appendChild(head);
      if (step.digest && step.digest.trim())
        card.appendChild(el("pre", "out", esc(cleanDigest(step.digest))));
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
    groupGate(bar, tail);
  } else {
    return null;
  }
  return bar;
}

// The capability gate: bough asks to grant filesystem access a step was denied.
// Present each candidate group with its description and a click-through to its
// exact paths, so granting access is an informed choice rather than approving an
// opaque name.
function groupGate(bar, tail) {
  bar.appendChild(el("h4", null, "🔑 Capability — a step needs access it doesn't have"));
  bar.appendChild(el("div", "gate-lead",
    `A sandboxed step was denied access to <code>${esc(tail.detail)}</code>. ` +
    `Enabling a group below grants that access for this session, then bough retries the step.`));

  ensureGroupsCatalog();
  const byName = new Map((state.groupsCatalog || []).map((g) => [g.name, g]));
  const names = (tail.groups || "").split(",").map((s) => s.trim()).filter(Boolean);

  const list = el("div", "gate-groups");
  for (const n of names) {
    const g = byName.get(n);
    const item = el("label", "gate-group");
    const cb = el("input"); cb.type = "checkbox"; cb.checked = true; cb.dataset.group = n;
    item.appendChild(cb);
    const meta = el("div", "gg-meta");
    meta.innerHTML = `<b>${esc(n)}</b>` +
      (g && g.description ? `<div class="gg-desc">${esc(g.description)}</div>` : "");
    item.appendChild(meta);
    const inspect = el("button", "gg-inspect", "paths ›");
    inspect.type = "button";
    inspect.onclick = (e) => { e.preventDefault(); e.stopPropagation(); inspectGroup(n); };
    item.appendChild(inspect);
    list.appendChild(item);
  }
  bar.appendChild(list);

  const row = el("div", "row");
  row.innerHTML =
    `<button class="accept" data-act="enable-groups">Enable &amp; retry</button>` +
    `<button class="reject" data-act="reject">Reject</button>`;
  bar.appendChild(row);
}

// Load the capability catalog once (for the gate's group descriptions), then
// re-render so the descriptions appear.
function ensureGroupsCatalog() {
  if ((state.groupsCatalog && state.groupsCatalog.length) || state._catalogLoading) return;
  state._catalogLoading = true;
  api.groupsCatalog()
    .then((g) => { state.groupsCatalog = g; state._catalogLoading = false; render(); })
    .catch(() => { state._catalogLoading = false; });
}

function renderTranscript() {
  const box = $("#transcript");
  box.innerHTML = "";

  // Subagent view: a back bar + the child's transcript.
  if (state.viewChildId) {
    const meta = (state.subagents || []).find((s) => s.id === state.viewChildId);
    const title = meta ? meta.title : state.viewChildId;
    const status = state.childRun ? state.childRun.status : (meta ? meta.status : "");
    const live = ACTIVE.has(status);
    const bar = el("div", "subbar");
    bar.innerHTML =
      `<button class="ghost" data-act="back-parent">‹ back</button>` +
      `<span class="sb-title" title="${esc(state.viewChildId)}">${esc(title)}</span>` +
      `<span class="st ${live ? "running" : esc(status || "done")}">${esc(status || "—")}</span>` +
      `<span class="sb-hint">${live ? "type below to message this subagent" : "this subagent has finished"}</span>`;
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
      // Carry the entry id so a step card can anchor a map "jump to" (see
      // jumpToEntry); it rides along harmlessly through the step pipeline.
      try { const s = JSON.parse(e.content); s._eid = e.id; steps.push(s); } catch {}
    } else if (e.role === "user") {
      flush();
      const m = el("div", "msg user", esc(e.content)); m.dataset.eid = e.id;
      stream.appendChild(m);
    } else if (e.role === "assistant") {
      // Guard against an older bug where the final reply was also stored as a
      // trailing plan step: drop a plan/text step identical to the answer so it
      // isn't shown twice.
      const ans = (e.content || "").trim();
      steps = steps.filter((s) => !((s.type === "plan" || s.type === "text") && (s.text || "").trim() === ans));
      flush();
      const m = el("div", "msg assistant prose", md(e.content)); m.dataset.eid = e.id;
      stream.appendChild(m);
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
    else {
      // The spinner stays in the stream; the Stop control lives on the composer.
      const card = el("div", "card plan live",
        `<span class="spin"><span class="pulse"></span> ${esc(growthLabel(run.status))}</span>`);
      stream.appendChild(card);
    }
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
  if (state.mapOpen) renderMap(); // reflect graft-arming / actions taken in the inspector
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
    body.appendChild(preField("output", cleanDigest(exec.digest)));
  // The entry id rides on the parsed step (set in renderConversation); when
  // present, you can branch the conversation off this exact tool call.
  const eid = (call && call._eid) || (exec && exec._eid);
  let acts = null;
  if (eid) {
    acts = el("div", "drawer-actions");
    const fork = el("button", "primary", "⑂ Branch off this step");
    fork.onclick = () => { forkNode(eid); closeDrawer(); closeMap(); };
    acts.appendChild(fork);
  }
  openDrawer("step", verb.toLowerCase() === "code" ? "code-mode step" : verb + " step", body, acts);
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
  if (onActivePath(node.id)) {
    const jump = el("button", "ghost", "↡ Jump to in transcript");
    jump.onclick = () => { closeDrawer(); jumpToEntry(node.id); };
    acts.appendChild(jump);
  }
  const fork = el("button", "primary", "Fork from here");
  fork.onclick = () => { forkNode(node.id); closeDrawer(); closeMap(); };
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
    // Drop repeated "waiting for subagents" status lines — keep only the last
    // one (a live run still shows it's waiting; the rest are just noise).
    if (s.type === "text" && (s.text || "").includes(WAIT_LINE)
      && i !== steps.length - 1) continue;
    // A review is a request + an outcome; when the model accepts immediately
    // they land back-to-back, so collapse to just the outcome chip.
    if (s.type === "review" && next && next.type === "review") continue;

    let card = null, advance = 0;
    if (s.type === "call" && next && next.type === "exec") {
      // A `collect` is a non-blocking status probe, not workspace work — render
      // it as one quiet chip (id · status) so polling never grows into a wall
      // of cards. Everything else pairs into a full call+exec card.
      card = (s.verb || "").toUpperCase() === "COLLECT"
        ? collectChip(next) : mergedCard(s, next);
      advance = 1; // consumed the exec
    } else {
      card = stepCard(s);
    }
    if (card) {
      // Fold a check that immediately follows onto this card as a ✓/✗ verdict,
      // so a round's work and its acceptance check read as one block instead of
      // a code→check ladder. (Only onto real cards, not chips.)
      const after = steps[i + 1 + advance];
      if (after && after.type === "check" && card.classList.contains("card")) {
        attachCheck(card, after);
        advance += 1;
      }
      if (s._eid) card.dataset.eid = s._eid; // anchor for map jump-to
      box.appendChild(card);
    }
    i += advance;
  }
}

// A small ✓/✗ verdict pill on a card's header — the round's acceptance check,
// folded onto the work that produced it. The check command's output is on hover.
function attachCheck(card, check) {
  const head = card.querySelector(".head");
  if (!head) return;
  const v = el("span", "checkmark " + (check.ok ? "ok" : "bad"), check.ok ? "✓ check" : "✗ check");
  v.title = (check.digest && check.digest.trim()) ? cleanDigest(check.digest) : "acceptance check";
  head.appendChild(v);
}

// A `collect` probe as a compact chip: subagent id + a one-word status, parsed
// from the exec digest (running / finished / failed), instead of a full card
// with the verbose "you don't need to wait" body.
function collectChip(exec) {
  const d = exec.digest || "";
  let status = "checked", cls = "";
  if (/is still running/.test(d)) status = "running";
  else if (/has finished/.test(d)) status = "finished";
  else if (/failed/.test(d)) { status = "failed"; cls = " bad"; }
  else if (/No subagent/.test(d)) { status = "unknown id"; cls = " bad"; }
  const m = d.match(/Subagent (\S+)/);
  const id = m ? m[1] : "";
  return el("div", "chip collect" + cls,
    `<span class="ic">↳</span> collect <span class="arg">${esc(id)}</span> · ${status}`);
}

// A delivered subagent result as a labeled card, collapsed to its title by
// default — the report body is often long, so it shouldn't dominate the
// transcript. Click the header to expand it inline.
function subagentReport(text) {
  const nl = text.indexOf("\n");
  const firstLine = (nl === -1 ? text : text.slice(0, nl));
  const body = (nl === -1 ? "" : text.slice(nl + 1)).trim();
  const m = firstLine.match(/Subagent "([^"]+)"/);
  const title = m ? m[1] : "subagent";
  const card = el("div", "card subreport collapsible collapsed");
  const head = el("div", "head");
  head.appendChild(el("span", "caret", "▸"));
  head.appendChild(el("span", "verb subagent", "↩ subagent"));
  head.appendChild(el("span", "arg", esc(title)));
  card.appendChild(head);
  if (body) card.appendChild(el("div", "subreport-body prose", md(body)));
  head.addEventListener("click", () => {
    head.querySelector(".caret").textContent =
      card.classList.toggle("collapsed") ? "▸" : "▾";
  });
  return card;
}

// The harness re-emits "… waiting for subagents to report" each time it parks
// to wait, so a multi-wave delegation stacks the same line many times. Keep it
// only as the live trailing indicator.
const WAIT_LINE = "waiting for subagents to report";

// A call+exec pair as one clickable card: verb, arg, exit, and an output
// preview. Click opens the drawer with the full program + full output.
// `code` and `spawn` carry the bulkiest bodies (a whole program, a spawn
// blurb), so they collapse to just their header by default — click the head to
// peek the output inline, click the output to open the full drawer.
const COLLAPSE_VERBS = new Set(["code", "spawn"]);
function mergedCard(call, exec) {
  const verb = (call.verb || "").toLowerCase();
  const ok = exec.exit === 0;
  const card = el("div", "card " + (ok ? "ok" : "bad"));
  const head = el("div", "head");
  head.appendChild(el("span", "verb " + verb, esc(call.verb)));
  head.appendChild(el("span", "arg", esc(call.arg || "")));
  // `spawn`/`tell` are async — there is no command exit code, so the "exit 0"
  // badge is just misleading noise. Only show it for steps that actually ran.
  if (verb !== "spawn" && verb !== "tell")
    head.appendChild(el("span", "exit " + (ok ? "ok" : "bad"), "exit " + exec.exit));
  card.appendChild(head);
  const hasBody = exec.digest && exec.digest.trim();
  if (hasBody) card.appendChild(el("pre", "out", esc(cleanDigest(exec.digest))));
  card.onclick = () => inspectStep(call, exec);
  if (hasBody && COLLAPSE_VERBS.has(verb)) makeCollapsible(card, head);
  return card;
}

// Collapse a card to its header by default; clicking the header toggles the
// inline body (and is swallowed so it doesn't also open the drawer).
function makeCollapsible(card, head) {
  card.classList.add("collapsible", "collapsed");
  const caret = el("span", "caret", "▸");
  head.insertBefore(caret, head.firstChild);
  head.addEventListener("click", (e) => {
    e.stopPropagation();
    caret.textContent = card.classList.toggle("collapsed") ? "▸" : "▾";
  });
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
    const meter = contextMeter(state.run ? state.run.context_tokens : 0, state.config.model);
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

// Context-window gauge: how full the model's context is after the last turn.
// `context_tokens` is the last turn's input+output (engine.gleam); the window is
// estimated per model family (override exact value isn't worth a round-trip).
function contextMeter(tokens, model) {
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
function contextWindow(model) {
  const m = (model || "").toLowerCase();
  if (/gemini/.test(m)) return 1000000;
  if (/claude/.test(m)) return 200000;
  if (/glm/.test(m)) return 200000;
  if (/gpt-5|gpt-4\.1|o[1-4]\b/.test(m)) return 200000;
  if (/gpt-4o|gpt-4/.test(m)) return 128000;
  return 128000; // llama/mistral/qwen/deepseek and unknowns
}
function humanTok(n) {
  if (n < 1000) return String(n);
  const k = n / 1000;
  return (k >= 100 ? Math.round(k) : Math.round(k * 10) / 10) + "k";
}

const projBase = (p) => (p || "").split("/").filter(Boolean).pop() || p || "untitled";

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

function toggleProject(key) {
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

function renderRight() {
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
  const body = $("#tabbody");
  body.innerHTML = "";
  if (!state.tree) { body.appendChild(el("div", "hint", "Open or create a session.")); return; }
  if (state.rightTab === "tree") renderTree(body);
  else if (state.rightTab === "changes") renderChanges(body);
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

const MAP = { NODE_W: 188, NODE_H: 52, X: 216, Y: 108, PAD: 64 };
const clampScale = (s) => Math.max(0.15, Math.min(2.6, s));

function openMap() {
  if (!state.tree) { toast("Open a session first.", true); return; }
  state.mapOpen = true;
  state.mapView = null; // fit on first paint
  state.mapExpanded = new Set();
  renderMap();
  requestAnimationFrame(fitMap); // fit needs the canvas laid out in the DOM
}

const parseStep = (content) => { try { return JSON.parse(content); } catch { return {}; } };
function onActivePath(eid) { return activePath(state.tree).some((e) => e.id === eid); }

// The tool-call rows shown when a turn is expanded — mirroring what the
// transcript actually renders (call+exec merged, empty/duplicate plans dropped)
// so every row's `eid` matches a real `[data-eid]` anchor to jump to.
function turnStepRows(stepEntries, turnEntry) {
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
function jumpToEntry(eid) {
  closeMap();
  const node = document.querySelector(`#transcript [data-eid="${eid}"]`);
  if (!node) { toast("That's on another branch — Fork to switch to it.", true); return; }
  node.scrollIntoView({ behavior: "smooth", block: "center" });
  node.classList.remove("flash"); void node.offsetWidth; node.classList.add("flash");
  setTimeout(() => node.classList.remove("flash"), 1700);
}

function toggleMapExpand(id) {
  if (!state.mapExpanded) state.mapExpanded = new Set();
  if (state.mapExpanded.has(id)) state.mapExpanded.delete(id);
  else state.mapExpanded.add(id);
  renderMap();
}

function closeMap() {
  state.mapOpen = false;
  const m = $("#mapview");
  if (m) m.remove();
}

// Collapse the raw entry tree to the *conversation* tree (user + assistant
// nodes), linking each visible node to its nearest visible ancestor — the same
// rule the outline uses, so the two views agree.
function visibleGraph(tree, showSup) {
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
function branchOrder(vchildren, tree) {
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
function treeBranches(tree) {
  if (!tree || !tree.entries || !tree.entries.length) return [];
  const { vnodes, vchildren } = visibleGraph(tree, false);
  const activeIds = new Set(activePath(tree).map((e) => e.id));
  const leaves = [...vnodes.values()].filter((n) => !(vchildren.get(n.id) || []).length);
  const branches = leaves.map((n) => {
    let turns = 0, cur = n;
    while (cur) { turns++; cur = cur.parent ? vnodes.get(cur.parent) : null; }
    return {
      leafId: n.id,
      name: (n.entry.label || "").trim() || clip(n.entry.content, 38) || "branch",
      named: !!(n.entry.label || "").trim(),
      turns,
      active: activeIds.has(n.id),
      trunk: n.id === tree.trunk_leaf,
      // A pending user-entry tip means a run is in flight on this branch (the
      // turn lands in the tree only on completion).
      running: n.entry.role === "user",
    };
  });
  branches.sort((a, b) => (a.active === b.active ? 0 : a.active ? -1 : 1));
  return branches;
}

// True while any branch has a run in flight, so the poller keeps the sidebar
// dots and the tree live even when you've navigated to an idle branch.
function anyBranchRunning(tree) {
  return treeBranches(tree).some((b) => b.running);
}

// Switch the active branch to a leaf — reuses fork (set active_leaf + restore
// that leaf's filesystem snapshot). No-op if it's already the active branch.
async function switchBranch(leafId) {
  if (!leafId || !state.tree || leafId === state.tree.active_leaf) return;
  await forkNode(leafId);
}

// Adopt a branch as trunk: restore the project dir to it and move the trunk
// pointer. The one place the working tree changes on purpose.
async function adoptBranch(leafId) {
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
function renameBranch(row, b) {
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

function layoutGraph(vchildren, tree) {
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

function buildMapShell() {
  if ($("#mapview")) return;
  const m = el("div");
  m.id = "mapview";
  m.innerHTML =
    `<div class="map-bar">
       <div class="map-bar-l">
         <span class="map-title"></span>
         <span class="map-hint"></span>
       </div>
       <div class="map-bar-r">
         <label class="map-sup"><input type="checkbox" id="map-sup"> superseded</label>
         <button class="ghost" data-map="fit" title="Fit to view">Fit</button>
         <span class="map-zoom"><button data-map="zout" aria-label="Zoom out">−</button><span class="map-pct">100%</span><button data-map="zin" aria-label="Zoom in">+</button></span>
         <button class="ghost" data-map="close" title="Close map (Esc)">✕ Close</button>
       </div>
     </div>
     <div class="map-canvas"><div class="map-world"><svg class="map-edges"></svg></div></div>`;
  document.body.appendChild(m);

  const canvas = m.querySelector(".map-canvas");
  // Drag the empty canvas to pan; nodes keep their own click.
  let drag = false, sx = 0, sy = 0, ox = 0, oy = 0;
  canvas.addEventListener("pointerdown", (e) => {
    if (e.target.closest(".map-node, .mnode-steps")) return;
    drag = true; canvas.setPointerCapture(e.pointerId);
    sx = e.clientX; sy = e.clientY;
    ox = state.mapView ? state.mapView.x : 0;
    oy = state.mapView ? state.mapView.y : 0;
    canvas.classList.add("grabbing");
  });
  canvas.addEventListener("pointermove", (e) => {
    if (!drag || !state.mapView) return;
    state.mapView.x = ox + (e.clientX - sx);
    state.mapView.y = oy + (e.clientY - sy);
    applyMapTransform();
  });
  const end = () => { drag = false; canvas.classList.remove("grabbing"); };
  canvas.addEventListener("pointerup", end);
  canvas.addEventListener("pointercancel", end);
  // Zoom toward the cursor.
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const v = state.mapView; if (!v) return;
    const r = canvas.getBoundingClientRect();
    const mx = e.clientX - r.left, my = e.clientY - r.top;
    const ns = clampScale(v.scale * Math.exp(-e.deltaY * 0.0015));
    v.x = mx - (mx - v.x) * (ns / v.scale);
    v.y = my - (my - v.y) * (ns / v.scale);
    v.scale = ns;
    applyMapTransform();
  }, { passive: false });

  m.querySelector(".map-bar-r").addEventListener("click", (e) => {
    const b = e.target.closest("[data-map]"); if (!b) return;
    if (b.dataset.map === "close") closeMap();
    else if (b.dataset.map === "fit") fitMap();
    else if (b.dataset.map === "zin") zoomBy(1.2);
    else if (b.dataset.map === "zout") zoomBy(1 / 1.2);
  });
  m.querySelector("#map-sup").addEventListener("change", (e) => {
    state.mapShowSuperseded = e.target.checked; renderMap();
  });
}

function applyMapTransform() {
  const w = $("#mapview .map-world");
  const v = state.mapView; if (!w || !v) return;
  w.style.transform = `translate(${v.x}px, ${v.y}px) scale(${v.scale})`;
  const pct = $("#mapview .map-pct"); if (pct) pct.textContent = Math.round(v.scale * 100) + "%";
}

function zoomBy(f) {
  const canvas = $("#mapview .map-canvas"); const v = state.mapView;
  if (!canvas || !v) return;
  const r = canvas.getBoundingClientRect();
  const cx = r.width / 2, cy = r.height / 2;
  const ns = clampScale(v.scale * f);
  v.x = cx - (cx - v.x) * (ns / v.scale);
  v.y = cy - (cy - v.y) * (ns / v.scale);
  v.scale = ns;
  applyMapTransform();
}

function fitMap() {
  const canvas = $("#mapview .map-canvas");
  const world = $("#mapview .map-world");
  if (!canvas || !world) return;
  const w = world._w || 1, h = world._h || 1; // world size stashed by renderMap
  const r = canvas.getBoundingClientRect();
  const s = clampScale(Math.min((r.width - MAP.PAD * 2) / w, (r.height - MAP.PAD * 2) / h, 1.1));
  state.mapView = { scale: s, x: (r.width - w * s) / 2, y: Math.max(MAP.PAD, (r.height - h * s) / 2) };
  applyMapTransform();
}

function renderMap() {
  if (!state.mapOpen || !state.tree) return;
  buildMapShell();
  const tree = state.tree;

  $("#mapview .map-title").textContent =
    (tree.project || "").split("/").filter(Boolean).pop() + " · " + tree.entries.length + " nodes";
  $("#mapview .map-hint").innerHTML = state.graftRoot
    ? `grafting — click a parent for <b>${esc(clip(nodeLabel(state.graftRoot), 22))}</b>`
    : `drag to pan · scroll to zoom · click a turn to jump · a branch tip ● to switch · ▸ steps · ⑂ fork/graft`;
  $("#mapview #map-sup").checked = !!state.mapShowSuperseded;

  const { vnodes, vchildren } = visibleGraph(tree, state.mapShowSuperseded);
  const pos = layoutGraph(vchildren, tree);

  let maxX = 0, maxY = 0;
  for (const p of pos.values()) { maxX = Math.max(maxX, p.x); maxY = Math.max(maxY, p.y); }
  const worldW = maxX + MAP.NODE_W, worldH = maxY + MAP.NODE_H;

  const world = $("#mapview .map-world");
  world.style.width = worldW + "px"; world.style.height = worldH + "px";
  world._w = worldW; world._h = worldH;

  // Edges first (under the nodes).
  const svg = world.querySelector(".map-edges");
  svg.setAttribute("width", worldW); svg.setAttribute("height", worldH);
  svg.setAttribute("viewBox", `0 0 ${worldW} ${worldH}`);
  let paths = "";
  for (const [id, obj] of vnodes) {
    if (!obj.parent || !pos.has(obj.parent)) continue;
    const a = pos.get(obj.parent), b = pos.get(id);
    const x1 = a.x + MAP.NODE_W / 2, y1 = a.y + MAP.NODE_H;
    const x2 = b.x + MAP.NODE_W / 2, y2 = b.y;
    const my = (y1 + y2) / 2;
    const cls = "edge" + (obj.entry.grafted_from ? " grafted" : "");
    paths += `<path class="${cls}" d="M${x1},${y1} C${x1},${my} ${x2},${my} ${x2},${y2}"/>`;
  }
  svg.innerHTML = paths;

  // Active path drives jump-to; precompute the id set once.
  const activeIds = new Set(activePath(tree).map((x) => x.id));
  const expanded = state.mapExpanded || new Set();

  // Nodes + step popovers (rebuilt each render; svg stays).
  world.querySelectorAll(".map-node, .mnode-steps").forEach((n) => n.remove());
  for (const [id, obj] of vnodes) {
    const p = pos.get(id), e = obj.entry;
    const isTip = !((vchildren.get(id) || []).length);
    const node = el("div", "map-node" +
      (id === tree.active_leaf ? " leaf" : "") +
      (isTip ? " tip" : "") +
      (activeIds.has(id) ? " onpath" : "") +
      (id === state.graftRoot ? " graftroot" : "") +
      (e.grafted_from ? " grafted" : ""));
    node.style.left = p.x + "px"; node.style.top = p.y + "px"; node.style.width = MAP.NODE_W + "px";
    node.innerHTML =
      `<span class="role ${esc(e.role)}">${e.role === "user" ? "you" : "bgh"}</span>` +
      (e.grafted_from ? `<span class="gmark" title="grafted from another branch">↪</span>` : "") +
      `<span class="snippet">${esc(clip(e.content, 64))}</span>` +
      (isTip ? `<span class="tipdot" title="branch tip">●</span>` : "");

    // Hover action: open the inspector (fork / graft) without firing a jump.
    const insp = el("button", "mnode-act", "⑂");
    insp.title = "Fork / graft from here";
    insp.onclick = (ev) => { ev.stopPropagation(); inspectNode(e); };
    node.appendChild(insp);

    // Expand toggle: reveal the tool-call steps that produced this turn.
    const rows = turnStepRows(obj.steps || [], e);
    if (rows.length) {
      const exp = el("button", "mnode-exp" + (expanded.has(id) ? " on" : ""),
        `${expanded.has(id) ? "▾" : "▸"} ${rows.length} step${rows.length === 1 ? "" : "s"}`);
      exp.title = "Show the tool calls in this turn";
      exp.onclick = (ev) => { ev.stopPropagation(); toggleMapExpand(id); };
      node.appendChild(exp);
    }

    node.onclick = async () => {
      if (state.graftRoot && state.graftRoot !== id) graftOnto(id);
      else if (activeIds.has(id)) jumpToEntry(id);
      // A branch tip off the active path: single-click switches to it (parity
      // with the sidebar). Interior off-path nodes still open the inspector.
      else if (isTip) { await switchBranch(id); closeMap(); }
      else inspectNode(e);
    };
    world.appendChild(node);

    // Expanded step strip: a popover under the node; each row jumps to that step.
    if (expanded.has(id) && rows.length) {
      const pop = el("div", "mnode-steps");
      pop.style.left = p.x + "px"; pop.style.top = (p.y + MAP.NODE_H + 8) + "px"; pop.style.width = MAP.NODE_W + "px";
      for (const r of rows) {
        const row = el("div", "mstep t-" + esc(r.type));
        row.innerHTML = `<span class="mstep-tag">${esc(String(r.tag))}</span>` +
          `<span class="mstep-lbl">${esc(clip(r.label || r.type, 38))}</span>`;
        row.title = "Jump to this step in the transcript";
        row.onclick = (ev) => { ev.stopPropagation(); jumpToEntry(r.eid); };
        const fk = el("button", "mstep-fork", "⑂");
        fk.title = "Branch off this tool call";
        fk.onclick = (ev) => { ev.stopPropagation(); forkNode(r.eid); closeMap(); };
        row.appendChild(fk);
        pop.appendChild(row);
      }
      world.appendChild(pop);
    }
  }
  applyMapTransform();
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

// ---- Changes (diff review) ----------------------------------------------
// The agent writes straight to the workspace; this is the review surface — the
// uncommitted diff (modified + new files) so you can see what it actually did
// before keeping it. Lazy-loaded and cached per session in state.diff.

async function refreshDiff() {
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

function renderChanges(body) {
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

function countDiffLines(patch, sign) {
  let n = 0;
  for (const l of (patch || "").split("\n")) {
    if (l[0] === sign && !l.startsWith(sign.repeat(3))) n++;
  }
  return n;
}

// A unified diff as colored, line-per-block spans (adds green, dels red,
// hunks amber, file headers dim).
function renderDiff(patch) {
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

function renderNetwork(body) {
  const net = state.run && state.run.network ? state.run.network : [];
  const leashed = !!(state.config && state.config.net);

  const posture = el("div", "net-posture " + (leashed ? "leashed" : "blocked"));
  posture.innerHTML = leashed
    ? `<span class="dot">◉</span><div><b>Leashed</b> — default-deny allowlist; a denied request pauses for your approval.</div>`
    : `<span class="dot">⦸</span><div><b>Blocked</b> — sandboxed commands have no network. Start with <code>BOUGH_NET=1</code> to leash instead.</div>`;
  body.appendChild(posture);

  if (net.length === 0) {
    body.appendChild(el("div", "hint", leashed
      ? "No requests itemized yet. Egress the engine observes appears here; code-mode bash is policy-enforced but isn't streamed (nono flushes its audit on session close)."
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
function renderPacks(body) {
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

async function refreshPacks() {
  try { state.packs = await api.packs(); } catch { state.packs = []; }
  if (state.rightTab === "caps") renderRight();
}

async function applyPack(name) {
  if (!state.sessionId) { toast("Open a session first.", true); return; }
  try {
    state.tree = await api.applyPacks(state.sessionId, [name]);
    toast(`Applied “${name}”`);
    render();
  } catch (e) { toast(String(e.message || e), true); }
}

async function deletePackByName(name) {
  try { await api.deletePack(name); await refreshPacks(); toast("Pack deleted"); }
  catch (e) { toast(String(e.message || e), true); }
}

function inspectPack(p) {
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

function listField(label, items) {
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
function savePackCurrent() {
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
function draftPackFlow() {
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
function packReview(description, draft) {
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

function renderCaps(body) {
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
function capsSection(body, sec, enabled, suggested, forceOpen) {
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
    name.onclick = () => inspectGroup(g.name);
    row.appendChild(name);
    body.appendChild(row);
  }
}

function renderSubagents(body) {
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

function subRow(s, live) {
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

function render() {
  renderHeader();
  renderSidebar();
  renderRight();
  renderTranscript();
  if (!state.viewChildId) dropSubbarIfPresentWithoutChild();
  const composer = $("#composer");
  const steering = !state.viewChildId && state.run && ACTIVE.has(state.run.status);
  composer.classList.toggle("steering", !!steering);
  // Show the Stop control while the viewed run (parent or subagent) is in flight.
  const activeRun = state.viewChildId ? state.childRun : state.run;
  $("#stop").hidden = !(activeRun && ACTIVE.has(activeRun.status));
  $("#prompt").placeholder = steering
    ? "Steer this run — type and Enter to inject…"
    : (state.viewChildId ? "Message this subagent…" : "Ask bough to do something…  (Enter to send, Shift+Enter for newline)");
  if (state.mapOpen) renderMap(); // keep the map in sync with new turns/forks/grafts
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
function ensurePoll() {
  const viewed = state.run && ACTIVE.has(state.run.status);
  if (viewed || anyBranchRunning(state.tree)) startPoll();
}

// Start a fresh session straight in a folder you already have — no form, since
// the project path is the folder header we clicked from.
async function newSessionInProject(proj) {
  if (!proj) return;
  try {
    const s = await api.createSession(proj);
    await loadSessions();
    await openSession(s.id);
  } catch (e) { toast(String(e.message || e), true); }
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

// ---- large-paste collapsing ---------------------------------------------
// A big paste (a log, a file) shouldn't bury the composer in a wall of text.
// We intercept it, keep it as a chip, and splice it back in on send.
const PASTE_MIN_LINES = 12, PASTE_MIN_CHARS = 1500;

function onPaste(e) {
  const cb = e.clipboardData || window.clipboardData;
  const t = cb ? cb.getData("text") : "";
  if (!t) return;
  const lines = t.split("\n").length;
  if (lines < PASTE_MIN_LINES && t.length < PASTE_MIN_CHARS) return; // small → paste inline
  e.preventDefault();
  state.pastes.push({ id: "p" + Date.now() + Math.random().toString(36).slice(2, 6), text: t, lines, chars: t.length });
  renderAttachments();
  toast(`Collapsed a ${lines.toLocaleString()}-line paste — sent with your message`);
}

function renderAttachments() {
  const box = $("#attachments");
  box.innerHTML = "";
  if (!state.pastes.length) { box.hidden = true; return; }
  box.hidden = false;
  for (const p of state.pastes) {
    const chip = el("div", "paste-chip");
    chip.innerHTML =
      `<span class="pc-ic">¶</span>` +
      `<span class="pc-label" data-act="paste-preview" data-id="${p.id}">` +
      `pasted · ${p.lines.toLocaleString()} lines · ${fmtBytes(p.chars)}</span>` +
      `<button class="pc-x" data-act="paste-remove" data-id="${p.id}" title="Remove">✕</button>`;
    box.appendChild(chip);
  }
}

function removePaste(id) { state.pastes = state.pastes.filter((p) => p.id !== id); renderAttachments(); }
function clearPastes() { state.pastes = []; renderAttachments(); }
function previewPaste(id) {
  const p = state.pastes.find((x) => x.id === id);
  if (!p) return;
  const body = el("div");
  body.appendChild(preField(`${p.lines.toLocaleString()} lines · ${fmtBytes(p.chars)}`, p.text));
  openDrawer("paste", "Pasted content", body);
}
const fmtBytes = (n) => n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KB" : (n / 1048576).toFixed(1) + " MB";

// The message actually sent: the typed text with any collapsed pastes appended.
function composeMessage() {
  const typed = $("#prompt").value.trim();
  if (!state.pastes.length) return typed;
  return [typed, ...state.pastes.map((p) => p.text)].filter(Boolean).join("\n\n");
}

async function submitComposer() {
  const ta = $("#prompt");
  const text = composeMessage();
  if (!text || !state.sessionId) return;
  const clear = () => { ta.value = ""; clearPastes(); closePicker(); };

  // Steering a subagent.
  if (state.viewChildId) {
    try { await api.control(state.viewChildId, "steer", text); clear(); toast("sent to subagent"); }
    catch (e) { toast(String(e.message || e), true); }
    return;
  }
  // Steering the live run.
  if (state.run && ACTIVE.has(state.run.status)) {
    try { await api.control(state.sessionId, "steer", text); clear(); toast("steering…"); }
    catch (e) { toast(String(e.message || e), true); }
    return;
  }
  // New run.
  clear();
  try {
    await api.startRun(state.sessionId, text, state.reviewArmed);
    state.tree = await api.tree(state.sessionId);
    state.run = { status: "running", steps: [], text: "", context_tokens: 0, network: [] };
    state.lastSig = null;
    render();
    startPoll();
  } catch (e) { toast(String(e.message || e), true); }
}

async function stopRun() {
  const id = state.viewChildId || state.sessionId;
  if (!id) return;
  try { await api.stop(id); toast("stopping — will halt at the next step"); }
  catch (e) { toast(String(e.message || e), true); }
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
    render();
    ensurePoll();
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
const PANES_KEY = "bough.panes";

function applyPanes() {
  let v = {};
  try { v = JSON.parse(localStorage.getItem(PANES_KEY) || "{}"); } catch {}
  document.body.classList.toggle("left-collapsed", !!v.left);
  document.body.classList.toggle("right-collapsed", !!v.right);
}

function togglePane(side) {
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

const picker = { open: false, items: [], active: 0, at: -1 };
const PICKER_MAX = 12;

async function ensureFiles() {
  if (state.files && state.files.sessionId === state.sessionId) return state.files.list;
  if (!state.sessionId) return [];
  try {
    const r = await api.files(state.sessionId);
    state.files = { sessionId: state.sessionId, list: r.files || [] };
  } catch {
    state.files = { sessionId: state.sessionId, list: [] };
  }
  return state.files.list;
}

// The "@token" the caret sits in: an "@" at the start of a word (preceded by
// start-of-line or whitespace), followed by non-whitespace up to the caret.
function atToken(ta) {
  const pos = ta.selectionStart;
  const m = ta.value.slice(0, pos).match(/(^|\s)@([^\s@]*)$/);
  if (!m) return null;
  return { at: pos - m[2].length - 1, query: m[2] };
}

// fzf-ish subsequence scorer: query chars must appear in order; reward
// contiguous runs, basename hits, and segment-start positions.
function fuzzyScore(query, path) {
  if (!query) return 1;
  const q = query.toLowerCase(), p = path.toLowerCase();
  const lastSlash = p.lastIndexOf("/");
  let qi = 0, score = 0, run = 0;
  for (let pi = 0; pi < p.length && qi < q.length; pi++) {
    if (p[pi] === q[qi]) {
      run++;
      score += run * 2;                                // contiguous matches compound
      if (pi > lastSlash) score += 3;                  // basename matters most
      if (pi === 0 || p[pi - 1] === "/") score += 4;   // segment starts
      qi++;
    } else run = 0;
  }
  if (qi < q.length) return -1;                        // not a subsequence
  return score - path.length * 0.05;                   // mild bias toward shorter paths
}

function fuzzyFilter(query, files) {
  const scored = [];
  for (const path of files) {
    const s = fuzzyScore(query, path);
    if (s >= 0) scored.push({ path, score: s });
  }
  scored.sort((a, b) => b.score - a.score || a.path.length - b.path.length);
  return scored.slice(0, PICKER_MAX);
}

async function refreshPicker() {
  const ta = $("#prompt");
  if (!atToken(ta)) { closePicker(); return; }
  const files = await ensureFiles();
  const now = atToken(ta); // the token may have changed while we awaited
  if (!now) { closePicker(); return; }
  picker.items = fuzzyFilter(now.query, files);
  picker.at = now.at;
  if (picker.items.length === 0) { closePicker(); return; }
  picker.open = true;
  picker.active = Math.min(picker.active, picker.items.length - 1);
  renderPicker(now.query);
}

function renderPicker(query) {
  const box = $("#filepicker");
  box.innerHTML = "";
  picker.items.forEach((it, i) => {
    const row = el("div", "fp-item" + (i === picker.active ? " active" : ""));
    row.setAttribute("role", "option");
    row.innerHTML = fpHighlight(it.path);
    row.onmousedown = (e) => { e.preventDefault(); choosePicker(i); };
    box.appendChild(row);
  });
  box.style.bottom = ($("#composer").offsetHeight + 6) + "px";
  box.hidden = false;
}

// Dim the directory, brighten the basename so the file you want stands out.
function fpHighlight(path) {
  const slash = path.lastIndexOf("/");
  const dir = slash >= 0 ? esc(path.slice(0, slash + 1)) : "";
  const base = esc(slash >= 0 ? path.slice(slash + 1) : path);
  return `<span class="fp-dir">${dir}</span><span class="fp-base">${base}</span>`;
}

function closePicker() {
  picker.open = false; picker.active = 0; picker.items = [];
  const box = $("#filepicker");
  if (box) { box.hidden = true; box.innerHTML = ""; }
}

function movePicker(delta) {
  if (!picker.open) return;
  const n = picker.items.length;
  picker.active = (picker.active + delta + n) % n;
  const tok = atToken($("#prompt"));
  renderPicker(tok ? tok.query : "");
  const active = $("#filepicker .fp-item.active");
  if (active) active.scrollIntoView({ block: "nearest" });
}

// Replace the "@query" token with the chosen path (plus a trailing space).
function choosePicker(i) {
  const it = picker.items[i];
  if (!it) return;
  const ta = $("#prompt");
  const before = ta.value.slice(0, picker.at);
  const after = ta.value.slice(ta.selectionStart);
  const insert = it.path + " ";
  ta.value = before + insert + after;
  const caret = before.length + insert.length;
  ta.setSelectionRange(caret, caret);
  closePicker();
  ta.focus();
}

// ---- event wiring --------------------------------------------------------

function wire() {
  // Tab switching.
  $("#tabs").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-tab]");
    if (!b) return;
    state.rightTab = b.dataset.tab;
    if (state.rightTab === "caps") {
      if (state.groupsCatalog.length === 0)
        api.groupsCatalog().then((g) => { state.groupsCatalog = g; renderRight(); }).catch(() => {});
      refreshPacks();
    }
    renderRight();
  });

  $("#new-session").addEventListener("click", newSession);
  $("#toggle-left").addEventListener("click", () => togglePane("left"));
  $("#toggle-right").addEventListener("click", () => togglePane("right"));
  $("#review-toggle").addEventListener("change", (e) => { state.reviewArmed = e.target.checked; });
  $("#session-search").addEventListener("input", (e) => { state.filter = e.target.value; renderSidebar(); });
  $("#scrim").addEventListener("click", closeDrawer);

  // Composer.
  $("#composer").addEventListener("submit", (e) => { e.preventDefault(); submitComposer(); });
  $("#stop").addEventListener("click", stopRun);
  $("#prompt").addEventListener("keydown", (e) => {
    if (picker.open) {
      if (e.key === "ArrowDown") { e.preventDefault(); movePicker(1); return; }
      if (e.key === "ArrowUp") { e.preventDefault(); movePicker(-1); return; }
      if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); choosePicker(picker.active); return; }
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closePicker(); return; }
    }
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); submitComposer(); }
  });
  $("#prompt").addEventListener("input", refreshPicker);
  $("#prompt").addEventListener("blur", () => setTimeout(closePicker, 120));
  $("#prompt").addEventListener("paste", onPaste);

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
      case "stop-run": stopRun(); break;
      case "toggle-project": toggleProject(t.dataset.proj); break;
      case "new-in-project": newSessionInProject(t.dataset.proj); break;
      case "pack-apply": applyPack(t.dataset.name); break;
      case "pack-delete": deletePackByName(t.dataset.name); break;
      case "pack-draft": draftPackFlow(); break;
      case "pack-save-current": savePackCurrent(); break;
      case "open-session": openSession(id); break;
      case "switch-branch": switchBranch(t.dataset.leaf); break;
      case "adopt-branch": adoptBranch(t.dataset.leaf); break;
      case "open-child": openChild(id); break;
      case "back-parent": backToParent(); break;
      case "graft-cancel": state.graftRoot = null; renderRight(); break;
      case "diff-refresh": state.diff = null; refreshDiff(); break;
      case "paste-remove": removePaste(id); break;
      case "paste-preview": previewPaste(id); break;
      case "toggle-done-subs": state.showDoneSubs = !state.showDoneSubs; renderRight(); break;
      case "toggle-caps-section": {
        const k = t.dataset.key;
        const cur = k in state.capsOpen ? state.capsOpen[k] : (k !== "alwayson");
        state.capsOpen[k] = !cur;
        renderRight();
        break;
      }
      case "allow": gateDecision("allow", ""); break;
      case "steer": gateDecision("steer", steerInput()); break;
      case "enable-groups": {
        const bar = t.closest(".gate");
        const picked = bar
          ? [...bar.querySelectorAll("input[type=checkbox][data-group]")].filter((c) => c.checked).map((c) => c.dataset.group)
          : [];
        if (picked.length === 0) { toast("Tick a group to enable, or Reject.", true); break; }
        gateDecision("steer", picked.join(","));
        break;
      }
      case "reject": gateDecision("steer", ""); break; // empty steer = deny (net/group)
      case "reject-plan": gateDecision("steer", steerInput() || "Reject this plan and revise the approach."); break;
    }
  });

  // Toggle a capability group (checkbox change).
  document.addEventListener("change", (e) => {
    const c = e.target.closest("[data-act=toggle-group]");
    if (c) toggleGroup(c.dataset.name, c.checked);
  });

  // Esc closes the inspector drawer first, then the map overlay.
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if ($("#drawer.open")) closeDrawer();
    else if (state.mapOpen) closeMap();
  });
}

// ---- boot ----------------------------------------------------------------

async function boot() {
  applyPanes();
  wire();
  try { state.config = await api.config(); } catch {}
  await loadSessions();
  if (state.sessions.length > 0) await openSession(state.sessions[0].id);
  else render();
}

boot();
