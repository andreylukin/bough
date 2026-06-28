import { forkNode, graftOnto } from "./actions.js";
import { beginEdit } from "./composer.js";
import { $, clip, el, esc, toast } from "./dom.js";
import { activePath, layoutGraph, nodeLabel, turnStepRows, visibleGraph } from "./graph.js";
import { switchBranch } from "./panes.js";
import { state } from "./state.js";
import { inspectNode } from "./transcript.js";

export const MAP = { NODE_W: 188, NODE_H: 52, X: 216, Y: 108, PAD: 64 };

export const clampScale = (s) => Math.max(0.15, Math.min(2.6, s));

export function openMap() {
  if (!state.tree) { toast("Open a session first.", true); return; }
  state.mapOpen = true;
  state.mapExpanded = new Set();
  renderMap();
  // Keep the camera you left when reopening within a session; only auto-fit when
  // there's no camera yet (first open, or a freshly-opened session reset it).
  if (!state.mapView) requestAnimationFrame(fitMap); // fit needs the canvas laid out
}

export function jumpToEntry(eid) {
  closeMap();
  const node = document.querySelector(`#transcript [data-eid="${eid}"]`);
  if (!node) { toast("That's on another branch — Fork to switch to it.", true); return; }
  node.scrollIntoView({ behavior: "smooth", block: "center" });
  node.classList.remove("flash"); void node.offsetWidth; node.classList.add("flash");
  setTimeout(() => node.classList.remove("flash"), 1700);
}

export function toggleMapExpand(id) {
  if (!state.mapExpanded) state.mapExpanded = new Set();
  if (state.mapExpanded.has(id)) state.mapExpanded.delete(id);
  else state.mapExpanded.add(id);
  renderMap();
}

export function closeMap() {
  state.mapOpen = false;
  const m = $("#mapview");
  if (m) m.remove();
}

// Collapse the raw entry tree to the *conversation* tree (user + assistant
// nodes), linking each visible node to its nearest visible ancestor — the same
// rule the outline uses, so the two views agree.

export function buildMapShell() {
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

export function applyMapTransform() {
  const w = $("#mapview .map-world");
  const v = state.mapView; if (!w || !v) return;
  w.style.transform = `translate(${v.x}px, ${v.y}px) scale(${v.scale})`;
  const pct = $("#mapview .map-pct"); if (pct) pct.textContent = Math.round(v.scale * 100) + "%";
}

export function zoomBy(f) {
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

export function fitMap() {
  const canvas = $("#mapview .map-canvas");
  const world = $("#mapview .map-world");
  if (!canvas || !world) return;
  const w = world._w || 1, h = world._h || 1; // world size stashed by renderMap
  const r = canvas.getBoundingClientRect();
  const s = clampScale(Math.min((r.width - MAP.PAD * 2) / w, (r.height - MAP.PAD * 2) / h, 1.1));
  state.mapView = { scale: s, x: (r.width - w * s) / 2, y: Math.max(MAP.PAD, (r.height - h * s) / 2) };
  applyMapTransform();
}

export function renderMap() {
  if (!state.mapOpen || !state.tree) return;
  buildMapShell();
  const tree = state.tree;

  $("#mapview .map-title").textContent =
    (tree.project || "").split("/").filter(Boolean).pop() + " · " + tree.entries.filter((e) => e.role === "user").length + " turns";
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
      if (state.graftRoot && state.graftRoot !== id) { graftOnto(id); return; }
      // Selecting one of your messages loads it into the composer to edit &
      // resend (sending then branches a new line of history).
      if (e.role === "user") { beginEdit(e); closeMap(); return; }
      if (activeIds.has(id)) jumpToEntry(id);
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
