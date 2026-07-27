/**
 * Tests for the one tabbed panel.
 *
 * Two properties carry this task, and they are the two the old tree got wrong:
 *
 * 1. **Tab switching is a state machine, not eight overlays.** Every way in and out —
 *    the toggle, a direct-jump chord, tab/shift-tab, escape — moves the same
 *    `PanelState`, and a chord works from a closed panel while arrows and enter do not.
 * 2. **A theme preview reverts when the tab is left without confirming** (spec §16:
 *    "browsing never commits"). The revert is asserted through the REDUCER, not by
 *    calling `cancel()` directly, because the defect this guards against is a departure
 *    path that forgot to call it. It is checked for every departure path there is.
 *
 * No JSX here on purpose: this file is `.ts`, so the components are rendered with
 * `createElement` against a fake, non-TTY stdout. They render from fixtures, with no
 * server, no store and no terminal (plan §7).
 */
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createElement, type ReactElement } from "react";
import { render, Text } from "ink";
import {
  initialPanel,
  McpTab,
  Panel,
  PANEL_TABS,
  type PanelAction,
  panelActionFor,
  type PanelState,
  reducePanel,
  SkillsTab,
  tabForChord,
  TABS,
  ThemeTab,
} from "./Panel.tsx";
import { PANEL_TOGGLE } from "../keys.ts";
import {
  applyTheme,
  createThemePreview,
  palette,
  presetIndex,
  stateFor,
  THEME_PRESETS,
  type ThemeState,
} from "../theme.ts";
import { chooseEntry, effectiveModel, modelEntries } from "./ModelPicker.tsx";
import { changeItems, diffBody, fileStats } from "./Changes.tsx";
import { labelFor, sessionItems } from "./Sessions.tsx";
import type { SessionRow } from "../api.ts";
import type { SessionChangeSet } from "../../server/changes.ts";
import { setColorEnabled } from "../format.ts";

setColorEnabled(false);

function draw(node: ReactElement, columns = 100): string {
  const out: string[] = [];
  const stdout = Object.assign(new EventEmitter(), {
    write: (s: string) => (out.push(s), true),
    columns,
    rows: 30,
    isTTY: false,
  });
  const stdin = Object.assign(new EventEmitter(), {
    isTTY: false,
    setRawMode() {},
    ref() {},
    unref() {},
    read: () => null,
    resume() {},
    pause() {},
  });
  const instance = render(node, {
    // deno-lint-ignore no-explicit-any -- ink types the streams as Node's own.
    stdout: stdout as any,
    // deno-lint-ignore no-explicit-any
    stdin: stdin as any,
    exitOnCtrlC: false,
    patchConsole: false,
  });
  instance.unmount();
  return out.join("");
}

// ---------------------------------------------------------------------------
// Tab switching
// ---------------------------------------------------------------------------

Deno.test("^t toggles the panel and every other chord jumps straight to its tab", () => {
  let state = initialPanel;
  assert.equal(state.open, false);

  const toggle = panelActionFor("panel.toggle");
  assert.deepEqual(toggle, { type: "toggle" });
  state = reducePanel(state, toggle!);
  assert.deepEqual(state, { open: true, tab: "sessions" });

  // A chord is a DIRECT jump: it works from any tab, and from a closed panel.
  for (const tab of TABS) {
    const action = panelActionFor(`tab.${tab.id}`);
    assert.deepEqual(action, { type: "jump", tab: tab.id }, tab.chord);
    const next = reducePanel({ open: false, tab: "sessions" }, action!);
    assert.deepEqual(next, { open: true, tab: tab.id });
  }

  // The chord that brought you here takes you back.
  const closed = reducePanel({ open: true, tab: "changes" }, { type: "jump", tab: "changes" });
  assert.deepEqual(closed, { open: false, tab: "changes" });
});

Deno.test("tab cycles the bar in both directions and wraps", () => {
  const first = PANEL_TABS[0];
  const last = PANEL_TABS[PANEL_TABS.length - 1];
  let state: PanelState = { open: true, tab: first };
  state = reducePanel(state, { type: "cycle", delta: 1 });
  assert.equal(state.tab, PANEL_TABS[1]);
  state = reducePanel({ open: true, tab: first }, { type: "cycle", delta: -1 });
  assert.equal(state.tab, last);
  state = reducePanel({ open: true, tab: last }, { type: "cycle", delta: 1 });
  assert.equal(state.tab, first);
});

Deno.test("panelActionFor claims the panel's commands and nothing else", () => {
  assert.deepEqual(panelActionFor("panel.close"), { type: "close" });
  assert.deepEqual(panelActionFor("panel.next"), { type: "cycle", delta: 1 });
  assert.deepEqual(panelActionFor("panel.prev"), { type: "cycle", delta: -1 });
  assert.deepEqual(panelActionFor("move.down"), { type: "move", delta: 1 });
  assert.deepEqual(panelActionFor("move.up"), { type: "move", delta: -1 });
  assert.deepEqual(panelActionFor("panel.confirm"), { type: "confirm" });
  // Chat's own commands pass straight through — the panel is not a key sink.
  assert.equal(panelActionFor("send"), null);
  assert.equal(panelActionFor("delete.wordBack"), null);
  assert.equal(panelActionFor("rail.enter"), null);
  // Nor are the workflow verbs: they belong to one TAB, and `PanelHost` routes them.
  assert.equal(panelActionFor("wf.pause"), null);
});

Deno.test("the keymap is data with no duplicate binding", () => {
  const chords = [PANEL_TOGGLE, ...TABS.map((t) => t.chord)];
  assert.equal(new Set(chords).size, chords.length, chords.join(","));
  assert.equal(new Set(TABS.map((t) => t.id)).size, TABS.length);
  assert.equal(tabForChord(PANEL_TOGGLE), null); // ^t is the toggle, never a tab
  assert.equal(tabForChord("zzz"), null);
});

Deno.test("the tab bar marks the active tab and the body follows it", () => {
  const bar = draw(createElement(Panel, { tab: "skills", rows: 12, skills: { skills: [] } }));
  for (const tab of TABS) assert.ok(bar.includes(tab.title), `${tab.title} missing`);
  assert.ok(bar.includes("no skills installed"), bar);

  const mcp = draw(createElement(Panel, { tab: "mcp", rows: 12 }));
  assert.ok(mcp.includes("loading…"), mcp);

  // A tab another task owns renders the slot it was handed, not an empty box.
  const slot = draw(
    createElement(Panel, { tab: "tree", rows: 8 }, createElement(Text, null, "the tree")),
  );
  assert.ok(slot.includes("the tree"), slot);
});

// ---------------------------------------------------------------------------
// Theme preview: browsing never commits
// ---------------------------------------------------------------------------

const START: ThemeState = { theme: null, defaults: {} };

Deno.test("a theme preview paints live and reverts when the tab is left", () => {
  const painted: (ThemeState | null)[] = [];
  const preview = createThemePreview({
    current: START,
    apply: (s) => painted.push(s),
  });
  let state: PanelState = { open: true, tab: "theme" };

  // Cursor movement previews — that is the whole point of the tab.
  state = reducePanel(state, { type: "move", delta: 1 }, { theme: preview });
  assert.equal(preview.previewing, true);
  assert.equal(preview.name, THEME_PRESETS[1].name);
  assert.equal(painted.length, 1);
  assert.equal(painted[0]?.theme?.name, THEME_PRESETS[1].name);

  // Leaving without confirming reverts to what was in force on entry.
  state = reducePanel(state, { type: "jump", tab: "sessions" }, { theme: preview });
  assert.deepEqual(state, { open: true, tab: "sessions" });
  assert.equal(preview.previewing, false);
  assert.deepEqual(painted[painted.length - 1], START);
  assert.equal(preview.index, presetIndex(START));
});

Deno.test("EVERY departure reverts — toggle, escape, chord, tab and shift-tab", () => {
  const departures: PanelAction[] = [
    { type: "toggle" },
    { type: "close" },
    { type: "jump", tab: "model" },
    { type: "cycle", delta: 1 },
    { type: "cycle", delta: -1 },
  ];
  for (const action of departures) {
    const painted: (ThemeState | null)[] = [];
    const preview = createThemePreview({ current: START, apply: (s) => painted.push(s) });
    reducePanel({ open: true, tab: "theme" }, { type: "move", delta: 2 }, { theme: preview });
    assert.equal(preview.previewing, true, JSON.stringify(action));
    reducePanel({ open: true, tab: "theme" }, action, { theme: preview });
    assert.equal(preview.previewing, false, JSON.stringify(action));
    assert.deepEqual(painted[painted.length - 1], START, JSON.stringify(action));
  }
});

Deno.test("enter keeps a theme, and leaving afterwards does not undo it", () => {
  const painted: (ThemeState | null)[] = [];
  const saved: string[] = [];
  const preview = createThemePreview({
    current: START,
    apply: (s) => painted.push(s),
    persist: (p) => saved.push(p.name),
  });
  reducePanel({ open: true, tab: "theme" }, { type: "move", delta: 1 }, { theme: preview });
  reducePanel({ open: true, tab: "theme" }, { type: "confirm" }, { theme: preview });
  assert.deepEqual(saved, [THEME_PRESETS[1].name]);
  assert.equal(preview.previewing, false);

  reducePanel({ open: true, tab: "theme" }, { type: "toggle" }, { theme: preview });
  const last = painted[painted.length - 1];
  assert.equal(last?.theme?.name, THEME_PRESETS[1].name);
  assert.equal(preview.name, THEME_PRESETS[1].name);
});

Deno.test("a preview repaints the REAL palette, and the revert restores it exactly", () => {
  const before = { ...palette };
  try {
    const preview = createThemePreview({ current: START }); // the shipped applyTheme
    const accent = palette.accent;
    const fjord = THEME_PRESETS.findIndex((p) => p.name === "Fjord");
    preview.select(fjord);
    assert.notEqual(palette.accent, accent);
    assert.equal(palette.accent, THEME_PRESETS[fjord].colors.green);
    reducePanel({ open: true, tab: "theme" }, { type: "close" }, { theme: preview });
    assert.equal(palette.accent, accent);
    // `epoch` only ever moves forward — it is the memo key, not a version.
    assert.ok(palette.epoch > before.epoch);
  } finally {
    applyTheme(null);
  }
});

Deno.test("the theme tab renders every preset and says which way is out", () => {
  const preview = createThemePreview({ current: START });
  const frame = draw(createElement(ThemeTab, { preview, rows: 20 }));
  for (const p of THEME_PRESETS) assert.ok(frame.includes(p.name), `${p.name} missing`);
  assert.ok(frame.includes("leaving the tab reverts"), frame);
  assert.ok(frame.includes("current: Default"), frame);
});

Deno.test("stateFor treats the empty partial as a reset, not as a named theme", () => {
  const reset = stateFor({ theme: { name: "Fjord", colors: { green: "#1" } }, defaults: {} }, {
    name: "Default",
    note: "",
    colors: {},
  });
  assert.equal(reset.theme, null);
});

// ---------------------------------------------------------------------------
// The tab bodies
// ---------------------------------------------------------------------------

Deno.test("the model picker sets BOTH tiers, and pins only this session", () => {
  const catalog = [
    { id: "claude-opus-5", label: "Opus 5", provider: "anthropic" as const },
    { id: "openai:gpt-5-mini", label: "GPT-5 mini", provider: "openai" as const },
  ];
  const entries = modelEntries(catalog);
  assert.deepEqual(
    [...new Set(entries.map((e) => e.tier))],
    ["frontier", "cheap", "effort"],
  );
  const cfg = {
    defaultModel: "claude-opus-5",
    sessionModel: null,
    cheapModel: null,
    defaultEffort: "default" as const,
    sessionEffort: null,
  };
  // Frontier: pins THIS session AND moves the default for new sessions (spec §12).
  const frontier = entries.find((e) => e.tier === "frontier" && e.id === "openai:gpt-5-mini")!;
  const after = chooseEntry(cfg, frontier);
  assert.equal(after.sessionModel, "openai:gpt-5-mini");
  assert.equal(after.defaultModel, "openai:gpt-5-mini");
  assert.equal(effectiveModel(after), "openai:gpt-5-mini");
  // …and the cheap tier is untouched by a frontier pick, and vice versa.
  assert.equal(after.cheapModel, null);
  const cheap = entries.find((e) => e.tier === "cheap" && e.id === "openai:gpt-5-mini")!;
  const both = chooseEntry(after, cheap);
  assert.equal(both.cheapModel, "openai:gpt-5-mini");
  assert.equal(both.sessionModel, "openai:gpt-5-mini");
  assert.equal(both.defaultModel, "openai:gpt-5-mini");
});

Deno.test("the changes tab says 'not a repository' rather than showing an empty diff", () => {
  const unavailable: SessionChangeSet = {
    available: false,
    reason: "this workspace is not a git repository",
    base: null,
    files: [],
    workspace: "/tmp/x",
  };
  assert.deepEqual(changeItems(unavailable), []);
  const frame = draw(
    createElement(Panel, {
      tab: "changes",
      rows: 12,
      changes: { set: unavailable, items: [], selected: 0, rows: 12 },
    }),
  );
  assert.ok(frame.includes("not a git repository"), frame);
  assert.equal(frame.includes("files changed"), false);
  assert.equal(frame.includes("no changes in this checkout yet"), false);
});

Deno.test("a change set counts its own +/- and flattens hunks for display", () => {
  const set: SessionChangeSet = {
    available: true,
    base: "abcdef1234",
    workspace: "/tmp/x",
    files: [{
      path: "src/tui/theme.ts",
      status: "modified",
      hunks: [{ header: "@@ -1,3 +1,4 @@", lines: [" keep", "-gone", "+new", "+also"] }],
    }],
  };
  assert.deepEqual(fileStats(set.files[0]), { added: 2, removed: 1 });
  assert.deepEqual(diffBody(set.files[0]), ["@@ -1,3 +1,4 @@", " keep", "-gone", "+new", "+also"]);
  // A binary file has no hunks: say so rather than render nothing.
  assert.deepEqual(diffBody({ path: "a.png", status: "added", hunks: [] }), [
    "(no textual diff — added)",
  ]);
  const items = changeItems(set);
  const frame = draw(
    createElement(Panel, {
      tab: "changes",
      rows: 16,
      changes: { set, items, selected: 0, rows: 16 },
    }),
  );
  assert.ok(frame.includes("theme.ts"), frame);
  assert.ok(frame.includes("+2"), frame);
  assert.ok(frame.includes("-1"), frame);
  assert.ok(frame.includes("since abcdef12"), frame);
});

Deno.test("the sessions tab lists conversations only, newest first, and filters", () => {
  const rows: SessionRow[] = [
    row("a", "root", { title: "wire the panel", createdAt: 1_000, originDir: "/src/bough" }),
    row("b", "fork", { title: "fork · retry the patch", createdAt: 3_000 }),
    row("c", "subagent", { title: "subagent · review", createdAt: 4_000 }),
    row("d", "workflow_agent", { title: "audit", createdAt: 5_000 }),
    row("e", "compaction", { title: "compacted · earlier", createdAt: 2_000 }),
  ];
  // Delegated kinds collapse under their origin (spec §4) — the tree tab shows them.
  assert.deepEqual(sessionItems(rows).map((i) => i.session.id), ["b", "e", "a"]);
  assert.equal(labelFor(rows[1]), "retry the patch");
  // The filter subtracts rows; it never reorders them.
  assert.deepEqual(sessionItems(rows, "patch").map((i) => i.session.id), ["b"]);
  assert.deepEqual(sessionItems(rows, "zzz"), []);

  const frame = draw(
    createElement(Panel, {
      tab: "sessions",
      rows: 14,
      sessions: {
        items: sessionItems(rows),
        selected: 0,
        currentId: "a",
        rows: 14,
        now: 10_000,
      },
    }),
  );
  assert.ok(frame.includes("retry the patch"), frame);
  assert.ok(frame.includes("here"), frame);
  assert.equal(frame.includes("review"), false); // the subagent is not listed
});

Deno.test("the MCP tab reports granted, connected and unauthorized distinctly", () => {
  const status = {
    registry: { servers: { alpha: { command: "alpha-server", args: [], env: {}, headers: {} } } },
    auth: { alpha: { authorized: false } },
    active: ["alpha"],
    connections: [],
  };
  const frame = draw(
    // deno-lint-ignore no-explicit-any -- a fixture of the four documented keys.
    createElement(McpTab, { status: status as any, selected: 0 }),
  );
  assert.ok(frame.includes("alpha"), frame);
  assert.ok(frame.includes("granted"), frame);
  assert.ok(frame.includes("needs auth"), frame);

  const empty = draw(createElement(SkillsTab, { skills: [], rows: 10 }));
  assert.ok(empty.includes("no skills installed"), empty);
  const one = draw(
    createElement(SkillsTab, {
      skills: [{ name: "history", description: "query the db", source: "bundled", dir: "/s" }],
      rows: 10,
    }),
  );
  assert.ok(one.includes("/history"), one);
  assert.ok(one.includes("query the db"), one);
});

function row(
  id: string,
  kind: SessionRow["kind"],
  over: Partial<SessionRow> = {},
): SessionRow {
  return {
    id,
    title: id,
    kind,
    createdAt: 0,
    parentId: null,
    busy: false,
    ...over,
  };
}
