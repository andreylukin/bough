import { startPoll } from "./actions.js";
import { api } from "./api.js";
import { $, el, esc, fmtBytes, toast } from "./dom.js";
import { render } from "./main.js";
import { ACTIVE, state } from "./state.js";
import { openDrawer, preField } from "./transcript.js";

export const PASTE_MIN_LINES = 12, PASTE_MIN_CHARS = 1500;

export function onPaste(e) {
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

export function renderAttachments() {
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

export function removePaste(id) { state.pastes = state.pastes.filter((p) => p.id !== id); renderAttachments(); }

export function clearPastes() { state.pastes = []; renderAttachments(); }

export function previewPaste(id) {
  const p = state.pastes.find((x) => x.id === id);
  if (!p) return;
  const body = el("div");
  body.appendChild(preField(`${p.lines.toLocaleString()} lines · ${fmtBytes(p.chars)}`, p.text));
  openDrawer("paste", "Pasted content", body);
}

export function composeMessage() {
  const typed = $("#prompt").value.trim();
  if (!state.pastes.length) return typed;
  return [typed, ...state.pastes.map((p) => p.text)].filter(Boolean).join("\n\n");
}

export async function submitComposer() {
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
    state.paneSig = null;
    render();
    startPoll();
  } catch (e) { toast(String(e.message || e), true); }
}

export const picker = { open: false, items: [], active: 0, at: -1 };

export const PICKER_MAX = 12;

export async function ensureFiles() {
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

export function atToken(ta) {
  const pos = ta.selectionStart;
  const m = ta.value.slice(0, pos).match(/(^|\s)@([^\s@]*)$/);
  if (!m) return null;
  return { at: pos - m[2].length - 1, query: m[2] };
}

// fzf-ish subsequence scorer: query chars must appear in order; reward
// contiguous runs, basename hits, and segment-start positions.

export function fuzzyScore(query, path) {
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

export function fuzzyFilter(query, files) {
  const scored = [];
  for (const path of files) {
    const s = fuzzyScore(query, path);
    if (s >= 0) scored.push({ path, score: s });
  }
  scored.sort((a, b) => b.score - a.score || a.path.length - b.path.length);
  return scored.slice(0, PICKER_MAX);
}

export async function refreshPicker() {
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

export function renderPicker(query) {
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

export function fpHighlight(path) {
  const slash = path.lastIndexOf("/");
  const dir = slash >= 0 ? esc(path.slice(0, slash + 1)) : "";
  const base = esc(slash >= 0 ? path.slice(slash + 1) : path);
  return `<span class="fp-dir">${dir}</span><span class="fp-base">${base}</span>`;
}

export function closePicker() {
  picker.open = false; picker.active = 0; picker.items = [];
  const box = $("#filepicker");
  if (box) { box.hidden = true; box.innerHTML = ""; }
}

export function movePicker(delta) {
  if (!picker.open) return;
  const n = picker.items.length;
  picker.active = (picker.active + delta + n) % n;
  const tok = atToken($("#prompt"));
  renderPicker(tok ? tok.query : "");
  const active = $("#filepicker .fp-item.active");
  if (active) active.scrollIntoView({ block: "nearest" });
}

// Replace the "@query" token with the chosen path (plus a trailing space).

export function choosePicker(i) {
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
