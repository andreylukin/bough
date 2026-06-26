import { applyPanes, backToParent, closeNav, gateDecision, loadSessions, newSession, newSessionInProject, openChild, openSession, stopRun, toggleGroup, togglePane } from "./actions.js";
import { api } from "./api.js";
import { choosePicker, closePicker, movePicker, onPaste, picker, previewPaste, refreshPicker, removePaste, submitComposer } from "./composer.js";
import { $, copyText, toast } from "./dom.js";
import { closeMap, renderMap } from "./map.js";
import { adoptBranch, applyPack, deletePackByName, draftPackFlow, refreshDiff, refreshPacks, renderHeader, renderRight, renderSidebar, savePackCurrent, switchBranch, toggleProject } from "./panes.js";
import { ACTIVE, state } from "./state.js";
import { closeDrawer, dropSubbar, renderTranscript } from "./transcript.js";

export function render() {
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

export function dropSubbarIfPresentWithoutChild() { dropSubbar(); }

// ---- actions -------------------------------------------------------------

export function wire() {
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
  $("#nav-scrim").addEventListener("click", closeNav);

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

  // Esc closes the inspector drawer first, then the map overlay, then any open
  // slide-over pane.
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if ($("#drawer.open")) closeDrawer();
    else if (state.mapOpen) closeMap();
    else closeNav();
  });
}

// ---- boot ----------------------------------------------------------------

export async function boot() {
  applyPanes();
  wire();
  try { state.config = await api.config(); } catch {}
  try { state.models = await api.models(); } catch {}
  await loadSessions();
  if (state.sessions.length > 0) await openSession(state.sessions[0].id);
  else render();
}

boot();
