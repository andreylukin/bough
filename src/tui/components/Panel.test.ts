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
 * `createElement` into OpenTUI's in-memory cell grid. They render from fixtures, with
 * no server, no store and no terminal (plan §7).
 */
import assert from "node:assert/strict";
import { test } from "bun:test";
import { testRender } from "@opentui/react/test-utils";
import { nameFromUrl } from "./PanelHost.tsx";
import { tabAtColumn } from "./Panel.tsx";
import { statusLegend } from "./Mcp.tsx";
import { createElement, type ReactNode } from "react";
import {
  initialPanel,
  McpTab,
  Panel,
  PANEL_TABS,
  type PanelAction,
  panelActionFor,
  type PanelState,
  type PanelTab,
  panelBodyRows,
  PanelTabs,
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
import { forestRows } from "../forest.ts";
import { Tree } from "./Tree.tsx";
import type { SessionRow } from "../api.ts";
import type { SessionChangeSet } from "../../server/changes.ts";
import { setColorEnabled } from "../format.ts";

setColorEnabled(false);

/**
 * The painted cells, as text.
 *
 * `captureCharFrame()` reads the render buffer back row by row, so what a test sees
 * is what a terminal would show. It is a FIXED GRID: anything past `columns` wraps
 * or is clipped, and anything past `rows` is simply not painted — so `rows` is sized
 * to the tallest thing a tab can produce rather than to a real terminal.
 */
async function draw(node: ReactNode, columns = 100, rows = 40): Promise<string> {
  // `testRender` mounts inside React's `act`, which is what makes the first commit
  // land before this returns — a bare `createRoot().render()` commits on a later
  // task and reads back an empty grid. The act ENVIRONMENT is then switched off:
  // nothing after the mount is act-wrapped, and leaving it on turns every later
  // repaint into a console warning.
  const t = await testRender(node, { width: columns, height: rows, exitOnCtrlC: false });
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  // `renderOnce` is MANDATORY: without a render pass the buffer holds uninitialised
  // glyphs rather than spaces, and every assertion below fails confusingly.
  await t.renderOnce();
  const frame = t.captureCharFrame();
  t.renderer.destroy();
  return frame;
}

// ---------------------------------------------------------------------------
// Tab switching
// ---------------------------------------------------------------------------

test("^t toggles the panel and every other chord jumps straight to its tab", () => {
  let state = initialPanel;
  assert.equal(state.open, false);

  const toggle = panelActionFor("panel.toggle");
  assert.deepEqual(toggle, { type: "toggle" });
  state = reducePanel(state, toggle!);
  // The tree is the home tab: it is the switcher AND the history, so it is what
  // `^t` with no further intent lands on.
  assert.deepEqual(state, { open: true, tab: "tree" });

  // A chord is a DIRECT jump: it works from any tab, and from a closed panel.
  for (const tab of TABS) {
    const action = panelActionFor(`tab.${tab.id}`);
    assert.deepEqual(action, { type: "jump", tab: tab.id }, tab.chord);
    const next = reducePanel({ open: false, tab: "tree" }, action!);
    assert.deepEqual(next, { open: true, tab: tab.id });
  }

  // The chord that brought you here takes you back.
  const closed = reducePanel({ open: true, tab: "changes" }, { type: "jump", tab: "changes" });
  assert.deepEqual(closed, { open: false, tab: "changes" });
});

test("tab cycles the bar in both directions and wraps", () => {
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

test("panelActionFor claims the panel's commands and nothing else", () => {
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

test("the keymap is data with no duplicate binding", () => {
  const chords = [PANEL_TOGGLE, ...TABS.map((t) => t.chord)];
  assert.equal(new Set(chords).size, chords.length, chords.join(","));
  assert.equal(new Set(TABS.map((t) => t.id)).size, TABS.length);
  assert.equal(tabForChord(PANEL_TOGGLE), null); // ^t is the toggle, never a tab
  assert.equal(tabForChord("zzz"), null);
});

test("the tab bar marks the active tab and the body follows it", async () => {
  const bar = await draw(createElement(Panel, { tab: "skills", rows: 12, skills: { skills: [] } }));
  for (const tab of TABS) assert.ok(bar.includes(tab.title), `${tab.title} missing`);
  assert.ok(bar.includes("no skills installed"), bar);

  const mcp = await draw(createElement(Panel, { tab: "mcp", rows: 12 }));
  assert.ok(mcp.includes("loading…"), mcp);

  // A tab another task owns renders the slot it was handed, not an empty box.
  const slot = await draw(
    createElement(Panel, { tab: "tree", rows: 8 }, createElement("text", null, "the tree")),
  );
  assert.ok(slot.includes("the tree"), slot);
});

// ---------------------------------------------------------------------------
// Theme preview: browsing never commits
// ---------------------------------------------------------------------------

const START: ThemeState = { theme: null, defaults: {} };

test("a theme preview paints live and reverts when the tab is left", () => {
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
  state = reducePanel(state, { type: "jump", tab: "changes" }, { theme: preview });
  assert.deepEqual(state, { open: true, tab: "changes" });
  assert.equal(preview.previewing, false);
  assert.deepEqual(painted[painted.length - 1], START);
  assert.equal(preview.index, presetIndex(START));
});

test("EVERY departure reverts — toggle, escape, chord, tab and shift-tab", () => {
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

test("enter keeps a theme, and leaving afterwards does not undo it", () => {
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

test("a preview repaints the REAL palette, and the revert restores it exactly", () => {
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

test("the theme tab renders every preset and says which way is out", async () => {
  const preview = createThemePreview({ current: START });
  const frame = await draw(createElement(ThemeTab, { preview, rows: 20 }));
  for (const p of THEME_PRESETS) assert.ok(frame.includes(p.name), `${p.name} missing`);
  // The legend is the tab's LAST row now, like every other tab's, and it still says
  // both halves: how to keep one, and that walking away puts the old one back.
  assert.ok(frame.includes("leaving reverts"), frame);
  assert.ok(frame.includes("current: Default"), frame);
  const rows = frame.split("\n").map((r) => r.trimEnd()).filter(Boolean);
  assert.match(rows[rows.length - 1], /current: Default/, frame);
});

test("stateFor treats the empty partial as a reset, not as a named theme", () => {
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

test("the model picker sets BOTH tiers, and pins only this session", () => {
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

test("the changes tab says 'not a repository' rather than showing an empty diff", async () => {
  const unavailable: SessionChangeSet = {
    available: false,
    reason: "this workspace is not a git repository",
    base: null,
    files: [],
    workspace: "/tmp/x",
  };
  assert.deepEqual(changeItems(unavailable), []);
  const frame = await draw(
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

test("a change set counts its own +/- and flattens hunks for display", async () => {
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
  // A file with no hunks says so rather than rendering nothing…
  assert.deepEqual(diffBody({ path: "a.png", status: "added", hunks: [] }), [
    "(no textual diff — added)",
  ]);
  // …and a BINARY one says which, now that `repodiff` flags it instead of decoding the
  // bytes as UTF-8 and diffing the replacement characters.
  assert.deepEqual(diffBody({ path: "a.png", status: "added", hunks: [], binary: true }), [
    "(binary file — added, contents not shown)",
  ]);
  // CONTROL BYTES NEVER REACH THE ROW. The rows render `line` verbatim, so an ESC in a
  // diff — byte soup with no NUL to flag it, a stray CR, a log with ANSI colour — was
  // obeyed by the terminal: a diff viewer moving the cursor and repainting the frame.
  // Tab survives; it is layout.
  assert.deepEqual(
    diffBody({
      path: "x.log",
      status: "modified",
      hunks: [{ header: "@@ -1 +1 @@", lines: ["+red \x1b[31mtext\x07\r", "+keeps\ttab"] }],
    }),
    ["@@ -1 +1 @@", "+red ·[31mtext··", "+keeps\ttab"],
  );
  const items = changeItems(set);
  const frame = await draw(
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

test("the tree tab lists conversations newest first, delegated work collapsed", async () => {
  const rows: SessionRow[] = [
    row("a", "root", { title: "wire the panel", createdAt: 1_000, originDir: "/src/bough" }),
    row("b", "root", { title: "nightly bench", createdAt: 3_000 }),
    row("c", "subagent", { title: "subagent · review", createdAt: 4_000, originId: "a" }),
  ];
  const items = forestRows({
    sessions: rows,
    childrenByOrigin: { a: [rows[2]] },
    threads: {},
    expanded: new Set(["a"]),
    drilled: new Set(),
    currentId: "a",
  });
  const frame = await draw(
    createElement(Panel, {
      tab: "tree",
      rows: 14,
    }, createElement(Tree, { rows: items, selected: 0, height: 12 })),
  );
  assert.ok(frame.includes("nightly bench"), frame);
  assert.ok(frame.includes("wire the panel"), frame);
  // Spec §4: the subagent is a COUNT, not a row of its own, until it is drilled into.
  assert.equal(frame.includes("review"), false, frame);
  assert.ok(frame.includes("1 delegated"), frame);
});

test("the MCP tab reports granted, connected and unauthorized distinctly", async () => {
  const status = {
    registry: { servers: { alpha: { command: "alpha-server", args: [], env: {}, headers: {} } } },
    auth: { alpha: { authorized: false } },
    active: ["alpha"],
    connections: [],
  };
  const frame = await draw(
    // A fixture of the four documented keys.
    createElement(McpTab, { status: status as any, selected: 0 }),
  );
  assert.ok(frame.includes("alpha"), frame);
  assert.ok(frame.includes("granted"), frame);
  assert.ok(frame.includes("needs auth"), frame);

  // A server carrying its own credential is NOT waiting for anyone to press `a`.
  // `sync-mcp` writes an Authorization header referencing the grant Claude Code
  // already holds, and this row said "needs auth" anyway — sending the user to
  // authorize the one server that needed nothing, where authorizing then fails
  // because the provider does not support dynamic registration.
  const withHeader = {
    registry: {
      servers: {
        slack: {
          url: "https://mcp.slack.com/mcp",
          args: [],
          env: {},
          headers: { Authorization: "Bearer ${keychain:Claude Code-credentials#mcpOAuth.s|1.accessToken}" },
        },
      },
    },
    auth: { slack: { authorized: false } },
    active: ["slack"],
    connections: [],
  };
  const authedFrame = await draw(
    createElement(McpTab, { status: withHeader as any, selected: 0 }),
  );
  assert.ok(authedFrame.includes("keychain"), authedFrame);
  assert.equal(authedFrame.includes("needs auth"), false, authedFrame);

  // Deleting a registration is reachable and advertised. `F` next door drops
  // CREDENTIALS and keeps the entry, so a server added by mistake — or a duplicate
  // pointing at an endpoint another entry already covers — used to be removable
  // only by hand-editing ~/.bough/mcp.json.
  assert.ok(authedFrame.includes("d delete"), authedFrame);
  // And proof: `c` connects now and reports, so "keychain" (which credential will
  // be TRIED) can be turned into an answer without spending a turn on a tool call.
  assert.ok(authedFrame.includes("c test"), authedFrame);
  assert.ok(authedFrame.includes("F forget"), authedFrame);

  const empty = await draw(createElement(SkillsTab, { skills: [], rows: 10 }));
  assert.ok(empty.includes("no skills installed"), empty);
  const one = await draw(
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

test("the open tab is marked in TEXT, not only in colour", async () => {
  // `setColorEnabled(false)` is in force for this file, which is the point: the
  // active tab used to be signalled by hue and weight alone, so a colourblind
  // reader, a NO_COLOR terminal and every text assertion in this repo all saw a
  // strip of eight identical words.
  for (const id of ["tree", "changes", "theme"] as PanelTab[]) {
    const text = await draw(createElement(PanelTabs, { tab: id }));
    assert.ok(text.includes(`[${id}]`), `tab "${id}" is not marked: ${text.trim()}`);
    // Exactly one tab is marked open at a time.
    assert.equal((text.match(/\[/g) ?? []).length, 1, text.trim());
  }
  // Every tab is still listed, marked or not.
  const strip = await draw(createElement(PanelTabs, { tab: "sessions" as PanelTab }));
  for (const t of TABS) assert.ok(strip.includes(t.title), `missing tab ${t.title}`);
});

// ---------------------------------------------------------------------------
// The row budget — the panel resize corruption
// ---------------------------------------------------------------------------

/**
 * THE REGRESSION THIS PINS.
 *
 * At 100x12 the sessions tab painted rows as character-level interleavings of two
 * different lines: `❯ ● ✓ wsvewsor28mGreeting Session  ws  4m` is two list rows on
 * one screen row. It was NOT stale cells and NOT React keying — it reproduced on a
 * fresh mount, because OpenTUI gives every auto-sized `<text>` `flexShrink: 1`, so a
 * tab body emitting six rows into a three-row box had all six SHRUNK to half a row
 * and pairs of them rounded onto the same y.
 *
 * The property that makes it impossible: what a tab paints is never taller than the
 * budget it was given. Asserted on the painted grid, at the height that broke, for
 * every tab — because the tabs each had their own arithmetic and each got it wrong
 * in its own way.
 */
const rowsOf = (frame: string) => frame.split("\n").map((r) => r.trimEnd());

const CATALOG = [
  { id: "claude-opus-5", label: "Opus 5", provider: "anthropic" as const },
  { id: "openai:gpt-5-mini", label: "GPT-5 mini", provider: "openai" as const },
];
const MODEL_CFG = {
  defaultModel: "claude-opus-5",
  sessionModel: null,
  cheapModel: null,
  defaultEffort: "default" as const,
  sessionEffort: null,
};
const LIST: SessionRow[] = Array.from({ length: 27 }, (_v, i) => ({
  id: `s${i}`,
  kind: "root",
  title: `session number ${i}`,
  workspace: "/tmp/ws",
  createdAt: 1_000 - i,
  updatedAt: 1_000 - i,
  lastTurnStatus: "done",
  busy: false,
  parentId: null,
} as SessionRow));

/** A glyph from a list row appearing on a row that is not a list row. */
function overdrawn(frame: string): string | null {
  for (const row of rowsOf(frame)) {
    // The legend and the counter are the two rows a list row used to land on top of.
    if (/[—-] \d+\/\d+ [—-].*[●⑂≣✓]/.test(row)) return row;
    if (/↑↓ move.*[●⑂≣]\s*[✓⋯]/.test(row)) return row;
  }
  return null;
}

test("no tab paints past its row budget — the 100x12 panel corruption", async () => {
  const items = forestRows({
    sessions: LIST,
    childrenByOrigin: {},
    threads: {},
    expanded: new Set(),
    drilled: new Set(),
  });

  // 12 terminal rows is what `App` leaves the panel roughly six of. Every height
  // from "absurdly cramped" up to comfortable must hold the property, because the
  // corruption appeared at some heights and not others.
  for (const h of [1, 2, 3, 4, 6, 8, 12, 20]) {
    for (const tab of PANEL_TABS) {
      const frame = await draw(
        createElement(Panel, {
          tab,
          rows: h,
          width: 100,
          changes: { set: null, items: [], selected: 0, rows: h },
          model: { cfg: MODEL_CFG, entries: modelEntries(CATALOG), selected: 0, rows: h },
          mcp: { status: null, selected: 0 },
          skills: { skills: null, selected: 0 },
          theme: { preview: createThemePreview({ current: START }) },
        }, createElement(Tree, { rows: items, selected: 0, height: panelBodyRows(h) })),
        100,
        // Two more than the panel's own box, so an overrun would be VISIBLE as a
        // painted row below the border rather than clipped by the grid.
        h + 4,
      );
      const painted = rowsOf(frame);
      // The panel is `rows + 2` tall (its border). Nothing may be painted below it.
      for (let i = h + 2; i < painted.length; i++) {
        assert.equal(painted[i], "", `${tab} @${h}: painted below the panel: ${frame}`);
      }
      // …and nothing may be painted ON it. At eight terminal rows the sessions
      // legend landed on the bottom border and rendered as
      // `╰─↑↓─move─·─pgup/pgdn─page─…─╯`, which is the same overrun one row up.
      assert.match(
        painted[h + 1] ?? "",
        /^╰─+╯$/,
        `${tab} @${h}: the bottom border was painted over: ${frame}`,
      );
      assert.equal(overdrawn(frame), null, `${tab} @${h}: two rows on one: ${frame}`);
    }
  }
});

test("the tree legend is the LAST row, at every height that has one", async () => {
  const items = forestRows({
    sessions: LIST,
    childrenByOrigin: {},
    threads: {},
    expanded: new Set(),
    drilled: new Set(),
  });
  for (const h of [4, 6, 8, 12, 20]) {
    const frame = await draw(
      createElement(Panel, {
        tab: "tree",
        rows: h,
        width: 100,
      }, createElement(Tree, { rows: items, selected: 0, height: panelBodyRows(h) })),
      100,
      h + 4,
    );
    const painted = rowsOf(frame).filter((r) => r.includes("│"));
    const last = painted[painted.length - 1] ?? "";
    assert.match(last, /↑↓ move/, `@${h}: the last row is not the legend: ${frame}`);
  }
});

// ---- registering an MCP server by URL ---------------------------------------
// The name is derived rather than asked for, so registration is ONE field. These
// pin the derivation, because a wrong name silently registers a second server
// beside one that may already be authorized.

test("a server name is derived from its URL, without the mcp. and the TLD", () => {
  assert.equal(nameFromUrl("https://mcp.linear.app/sse"), "linear");
  assert.equal(nameFromUrl("https://mcp.notion.com/mcp"), "notion");
  assert.equal(nameFromUrl("https://api.githubcopilot.com/mcp/"), "githubcopilot");
  assert.equal(nameFromUrl("https://example.com"), "example");
  // A port and a path change nothing — the host is the whole of the answer.
  assert.equal(nameFromUrl("http://localhost:3000/mcp"), "localhost");
});

test("a co.uk-shaped suffix does not become the name", () => {
  // Taking "the part before the TLD" naively would call this one "co".
  assert.equal(nameFromUrl("https://mcp.acme.co.uk/sse"), "acme");
});

test("a name that is taken gets a suffix rather than overwriting", () => {
  // Overwriting silently would replace a registration that may already hold
  // credentials — the one outcome worse than asking.
  assert.equal(nameFromUrl("https://mcp.linear.app/sse", ["linear"]), "linear-2");
  assert.equal(nameFromUrl("https://mcp.linear.app/sse", ["linear", "linear-2"]), "linear-3");
});

test("something that is not a URL yields no name, so nothing is registered", () => {
  assert.equal(nameFromUrl("linear"), "");
  assert.equal(nameFromUrl(""), "");
  assert.equal(nameFromUrl("https://"), "");
});

// ---- clicking the tab strip -------------------------------------------------
// The column math walks the SAME widths `PanelTabs` renders: an inactive tab is
// "  " + title, an active one is " [" + title + "]". Two derivations of that
// layout is how a click lands one tab off at the far end of the strip.

test("a click on a title picks that tab, on either side of the active one", () => {
  // "  tree  changes …" — the strip opens with two spaces, so `tree` occupies
  // columns 2..5.
  assert.equal(tabAtColumn("changes", 2), "tree");
  assert.equal(tabAtColumn("changes", 5), "tree");
  // Every tab is reachable, including the last, which is what the truncation bug
  // in `PanelTabs` was originally about.
  assert.equal(tabAtColumn("changes", 0), null);
  assert.equal(tabAtColumn("tree", 9), "changes");
});

test("the ACTIVE tab's brackets shift everything after it by one", () => {
  // " [tree]" is one column wider than "  tree", so a strip measured against the
  // inactive layout would drift right for every later tab. Column 8 is the
  // discriminator: with `tree` active its "]" pushes `changes` right to 9, leaving
  // 8 as padding; with `tree` inactive `changes` starts at 8.
  assert.equal(tabAtColumn("tree", 8), null);
  assert.equal(tabAtColumn("changes", 8), "changes");
});

test("a click in the padding between titles picks nothing", () => {
  // The gap belongs to neither neighbour. Rounding toward one of them is how a
  // click near an edge selects the tab you were trying to avoid.
  assert.equal(tabAtColumn("changes", 1), null);
  assert.equal(tabAtColumn("changes", 6), null);
});

test("every tab is reachable by some column", () => {
  // The complement of the above: a tab no column resolves to is a tab the mouse
  // cannot reach, however plausible the strip looks.
  for (const active of ["tree", "mcp"] as const) {
    const seen = new Set<string>();
    for (let c = 0; c < 200; c++) {
      const hit = tabAtColumn(active, c);
      if (hit) seen.add(hit);
    }
    assert.equal(seen.size, TABS.length, `active=${active}: reached ${[...seen].join(",")}`);
  }
});

test("the MCP tab says what its status glyphs mean, for the ones on screen", async () => {
  // Reported exactly as it feels: authorize a server, watch it stay a half circle,
  // and have no way to learn that `◐` is about a CONNECTION and not about the
  // authorization that just succeeded.
  const status = {
    registry: {
      servers: {
        live: { url: "https://a.example/mcp", args: [], env: {}, headers: {} },
        granted: { url: "https://b.example/mcp", args: [], env: {}, headers: {} },
      },
    },
    auth: {},
    active: ["live", "granted"],
    connections: [{ server: "live", alive: true, toolCount: 2 }],
  };
  assert.deepEqual(statusLegend(["live", "granted"], status as any), [
    "● connected",
    "◐ granted, not connected — c connects",
  ]);
  // Present-only: a list where everything is granted should not spend columns on `○`.
  assert.equal(
    statusLegend(["live", "granted"], status as any).some((l) => l.includes("○")),
    false,
  );
  const frame = await draw(createElement(McpTab, { status: status as any, selected: 0 }));
  assert.ok(frame.includes("granted, not connected"), frame);
});
