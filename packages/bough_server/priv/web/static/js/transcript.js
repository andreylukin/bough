import { forkNode, inspectGroup } from "./actions.js";
import { api } from "./api.js";
import { $, cleanDigest, clip, el, esc, md, toast } from "./dom.js";
import { activePath, onActivePath, stepLabel } from "./graph.js";
import { render } from "./main.js";
import { closeMap, jumpToEntry, renderMap } from "./map.js";
import { renderRight } from "./panes.js";
import { ACTIVE, state } from "./state.js";

export function stepCard(step) {
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
      const ok = step.exit === 0;
      const card = el("div", "card " + (ok ? "ok" : "bad"));
      const head = el("div", "head");
      head.appendChild(el("span", "tag worker", "worker fix"));
      head.appendChild(el("span", "arg", esc(clip(step.command || "", 90))));
      head.appendChild(el("span", "exit " + (ok ? "ok" : "bad"), "exit " + step.exit));
      card.appendChild(head);
      // Collapsed body: the brief the supervisor handed the worker (the plan)
      // and the fix it produced — so you can see what was asked, not just the
      // resulting command.
      const brief = (step.brief || "").trim();
      if (brief || step.command) {
        const body = el("div", "worker-body");
        if (brief) {
          body.appendChild(el("div", "wlabel", "plan from supervisor →"));
          body.appendChild(el("pre", "out", esc(cleanDigest(brief))));
        }
        if (step.command) {
          body.appendChild(el("div", "wlabel", "worker’s fix"));
          body.appendChild(el("pre", "out", esc(step.command)));
        }
        card.appendChild(body);
        makeCollapsible(card, head);
      }
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

export function gateBar(run, sessionId) {
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

export function groupGate(bar, tail) {
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

export function ensureGroupsCatalog() {
  if ((state.groupsCatalog && state.groupsCatalog.length) || state._catalogLoading) return;
  state._catalogLoading = true;
  api.groupsCatalog()
    .then((g) => { state.groupsCatalog = g; state._catalogLoading = false; render(); })
    .catch(() => { state._catalogLoading = false; });
}

export function renderTranscript() {
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

export let _subbar = null;

export function cleanupSubbar(bar) {
  if (_subbar && _subbar !== bar) _subbar.remove();
  _subbar = bar;
}

export function dropSubbar() { if (_subbar) { _subbar.remove(); _subbar = null; } }

export function renderConversation(box, tree, run) {
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

export function growthLabel(status) {
  return { running: "growing…", awaiting_plan: "waiting for you", awaiting_net: "waiting for you", awaiting_group: "waiting for you" }[status] || status + "…";
}

// ---- inspector drawer ----------------------------------------------------
// Any element you click opens this with everything known about it.

export function openDrawer(kind, title, bodyEl, actionsEl) {
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

export function closeDrawer() {
  const d = $("#drawer");
  if (!d.classList.contains("open")) return;
  d.classList.remove("open"); d.setAttribute("aria-hidden", "true");
  $("#scrim").classList.remove("show");
  if (state.lastFocus && state.lastFocus.focus) state.lastFocus.focus();
  if (state.mapOpen) renderMap(); // reflect graft-arming / actions taken in the inspector
}

export function kvField(label, rows) {
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

export function preField(label, text) {
  const f = el("div", "field");
  f.appendChild(el("div", "flabel", label));
  f.appendChild(el("pre", null, esc(text)));
  return f;
}

export function inspectStep(call, exec) {
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

export function inspectNode(node) {
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

export function inspectNet(ev) {
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

export function renderStepList(box, steps) {
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

export function attachCheck(card, check) {
  const head = card.querySelector(".head");
  if (!head) return;
  const v = el("span", "checkmark " + (check.ok ? "ok" : "bad"), check.ok ? "✓ check" : "✗ check");
  v.title = (check.digest && check.digest.trim()) ? cleanDigest(check.digest) : "acceptance check";
  head.appendChild(v);
}

// A `collect` probe as a compact chip: subagent id + a one-word status, parsed
// from the exec digest (running / finished / failed), instead of a full card
// with the verbose "you don't need to wait" body.

export function collectChip(exec) {
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

export function subagentReport(text) {
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

export const WAIT_LINE = "waiting for subagents to report";

// A call+exec pair as one clickable card: verb, arg, exit, and an output
// preview. Click opens the drawer with the full program + full output.
// `code` and `spawn` carry the bulkiest bodies (a whole program, a spawn
// blurb), so they collapse to just their header by default — click the head to
// peek the output inline, click the output to open the full drawer.

export const COLLAPSE_VERBS = new Set(["code", "spawn"]);
// First non-blank line of a program/content, for the collapsed-card preview.

export function firstLine(s) {
  const l = (s || "").split("\n").find((x) => x.trim());
  return clip(l || "", 72);
}
// What the supervisor wrote for a step — the plan — labeled by verb. Just
// `code` for now: its program is the plan and is otherwise only in the drawer.

export const PLAN_LABEL = { code: "program" };

export function mergedCard(call, exec) {
  const verb = (call.verb || "").toLowerCase();
  const ok = exec.exit === 0;
  const card = el("div", "card " + (ok ? "ok" : "bad"));
  const head = el("div", "head");
  head.appendChild(el("span", "verb " + verb, esc(call.verb)));
  // The program/content the supervisor planned (e.g. `code`'s body) — preview
  // its first line in the header so a collapsed card still says what it does.
  const detail = (call.detail || "").trim();
  const planVerb = verb in PLAN_LABEL && verb !== "spawn";
  head.appendChild(el("span", "arg", esc(call.arg || (planVerb ? firstLine(detail) : ""))));
  // `spawn`/`tell` are async — there is no command exit code, so the "exit 0"
  // badge is just misleading noise. Only show it for steps that actually ran.
  if (verb !== "spawn" && verb !== "tell")
    head.appendChild(el("span", "exit " + (ok ? "ok" : "bad"), "exit " + exec.exit));
  card.appendChild(head);
  // Body: the plan (the program/content the supervisor wrote), then its output.
  const out = exec.digest && exec.digest.trim() ? cleanDigest(exec.digest) : "";
  const body = el("div", "step-body");
  let hasBody = false;
  if (detail && planVerb) {
    body.appendChild(el("div", "slabel", PLAN_LABEL[verb] || "plan"));
    body.appendChild(el("pre", "out", esc(detail)));
    hasBody = true;
  }
  if (out) {
    if (hasBody) body.appendChild(el("div", "slabel", "output"));
    body.appendChild(el("pre", "out", esc(out)));
    hasBody = true;
  }
  if (hasBody) card.appendChild(body);
  card.onclick = () => inspectStep(call, exec);
  if (hasBody && COLLAPSE_VERBS.has(verb)) makeCollapsible(card, head);
  return card;
}

// Collapse a card to its header by default; clicking the header toggles the
// inline body (and is swallowed so it doesn't also open the drawer).

export function makeCollapsible(card, head) {
  card.classList.add("collapsible", "collapsed");
  const caret = el("span", "caret", "▸");
  head.insertBefore(caret, head.firstChild);
  head.addEventListener("click", (e) => {
    e.stopPropagation();
    caret.textContent = card.classList.toggle("collapsed") ? "▸" : "▾";
  });
}

// ---- rendering: header + sidebar ----------------------------------------
