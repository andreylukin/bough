/**
 * Tests that the panel is MOUNTED, not merely written.
 *
 * The defect this file exists to prevent is the one phase 3 shipped: `Panel.tsx`,
 * `Changes.tsx`, `ModelPicker.tsx` and `Sessions.tsx` all rendered correctly from
 * fixtures, all had passing tests, and none of them was reachable from the running
 * client — `App.tsx` imported none of them, so the whole chain was dead code with a
 * green suite. "It typechecks" and "it renders from a fixture" do not add up to "it is
 * in the product", so this file asserts the two things that do:
 *
 * 1. **The import graph.** `App.tsx` reaches `Panel.tsx` transitively, walked from the
 *    source text. An existence check would have passed the whole time it was dead.
 * 2. **A keypress reaches it.** `App` is mounted against a fake TTY and driven by raw
 *    control bytes, exactly as a terminal would deliver them. Each of the eight tabs
 *    spec §15 names is opened by its own chord and asserted to have painted.
 *
 * Everything is a fixture: a fake store, an in-memory stdout, no server, no socket,
 * no `~/.bough`.
 */
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { render } from "ink";
import type { ReactElement } from "react";
import { App } from "./App.tsx";
import { TABS } from "../keys.ts";
import { initialState, type Store, type TuiState } from "../store.ts";
import { setColorEnabled } from "../format.ts";
import type { SessionRow } from "../api.ts";
import type { ModelRow } from "../../llm/client.ts";
import type { McpStatus } from "../../mcp/status.ts";

setColorEnabled(false);

// ---------------------------------------------------------------------------
// 1. The import graph — the assertion that would have caught the dead chain
// ---------------------------------------------------------------------------

const HERE = new URL(".", import.meta.url);

/** Every relative specifier a module imports, in source order. */
function importsOf(source: string): string[] {
  const out: string[] = [];
  const re = /(?:from|import)\s*["'](\.[^"']+)["']/g;
  for (const m of source.matchAll(re)) out.push(m[1]);
  return out;
}

/** Modules reachable from `entry` by following relative imports, as file URLs. */
async function reachable(entry: URL): Promise<Set<string>> {
  const seen = new Set<string>();
  const queue = [entry.href];
  while (queue.length > 0) {
    const href = queue.pop()!;
    if (seen.has(href)) continue;
    seen.add(href);
    let source: string;
    try {
      source = await Deno.readTextFile(new URL(href));
    } catch {
      continue; // a package specifier resolved into node_modules; not our graph
    }
    for (const spec of importsOf(source)) queue.push(new URL(spec, href).href);
  }
  return seen;
}

Deno.test("the panel and every tab it holds are reachable from App", async () => {
  const graph = await reachable(new URL("App.tsx", HERE));
  const named = (file: string) => new URL(file, HERE).href;

  // The chain that was dead: App imported none of these, transitively or otherwise.
  for (
    const file of [
      "Panel.tsx",
      "PanelHost.tsx",
      "Sessions.tsx",
      "Changes.tsx",
      "ModelPicker.tsx",
      "Mcp.tsx",
      "Skills.tsx",
      "Theme.tsx",
      "Tree.tsx",
      "Workflows.tsx",
    ]
  ) {
    assert.ok(graph.has(named(file)), `${file} is not reachable from App.tsx`);
  }

  // …and the tab table is the keymap's, so there is exactly one of it.
  assert.ok(graph.has(new URL("../keys.ts", HERE).href));
  const panel = await Deno.readTextFile(new URL("Panel.tsx", HERE));
  assert.ok(panel.includes('from "../keys.ts"'), "Panel declares its own chord table");
});

// ---------------------------------------------------------------------------
// A mounted App, driven by raw bytes
// ---------------------------------------------------------------------------

/** The control byte a terminal sends for `^x`. This is what raw mode delivers. */
const ctrl = (letter: string) => String.fromCharCode(letter.toLowerCase().charCodeAt(0) - 96);

interface Harness {
  frame(): string;
  press(bytes: string): Promise<void>;
  unmount(): void;
}

async function mount(node: ReactElement, columns = 100, rows = 30): Promise<Harness> {
  // The LAST frame written, not everything since the last keypress. Ink skips the
  // write entirely when a render produces the same bytes, so "what was printed since
  // I pressed a key" is empty exactly when nothing changed — and an empty string
  // satisfies every `assert.equal(frame.includes(x), false)` for free. Holding the
  // current frame instead means a negative assertion is about what is ON SCREEN.
  // A frame may arrive as several writes, so it is reassembled on the synchronized-
  // output marker `term.ts` wraps every repaint in — that sequence is the frame
  // boundary the terminal itself uses.
  const SYNC_BEGIN = String.fromCharCode(27) + "[?2026h";
  let last = "";
  let frames = 0;
  const stdout = Object.assign(new EventEmitter(), {
    write: (s: string) => {
      if (s.includes(SYNC_BEGIN)) (last = s, frames++);
      else last += s;
      return true;
    },
    columns,
    rows,
    isTTY: true,
  });
  // A real terminal, as far as ink is concerned: `isTTY` (or `useInput` is inert and
  // this whole file proves nothing), and the pull-based `readable`/`read()` pair ink
  // actually consumes rather than the `data` event it stopped using.
  const pending: string[] = [];
  const stdin = Object.assign(new EventEmitter(), {
    isTTY: true,
    setRawMode() {},
    setEncoding() {},
    ref() {},
    unref() {},
    read: () => pending.shift() ?? null,
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
  // Ink attaches its `readable` listener and `useInput` subscribes to the parsed
  // input from EFFECTS, which React runs after the first paint. A key delivered in
  // the same tick as `render` is therefore read off the stream and dropped on the
  // floor — silently, which is why this is awaited here rather than papered over
  // with a retry: the first keypress of every test must count.
  await settle(() => frames > 0);
  return {
    frame: () => last,
    async press(bytes: string) {
      const before = frames;
      pending.push(bytes);
      stdin.emit("readable");
      await settle(() => frames > before);
    },
    unmount: () => instance.unmount(),
  };
}

/**
 * Wait for the next painted frame.
 *
 * Polled rather than slept, because ink's repaint is not on a fixed delay: a lone ESC
 * is held for 20ms to see whether a CSI sequence follows it, and a heavier tab takes
 * an extra pass. A fixed sleep long enough for the slowest of them makes the suite
 * crawl, and one tuned to the fastest reads a stale frame and blames the panel. When
 * a keypress genuinely paints nothing this returns after the ceiling, and the caller's
 * assertion is then about the frame that IS on screen.
 */
async function settle(painted: () => boolean): Promise<void> {
  for (let waited = 0; waited < 500 && !painted(); waited += 10) {
    await new Promise((r) => setTimeout(r, 10));
  }
  // One more pass: a paint can be followed by a second render (an effect's fetch
  // landing), and the assertion should see where it settled, not where it passed.
  await new Promise((r) => setTimeout(r, 30));
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const MODELS: ModelRow[] = [
  { id: "claude-opus-5", label: "Opus 5", provider: "anthropic" },
  { id: "openai:gpt-5-mini", label: "GPT-5 mini", provider: "openai" },
];

const MCP: McpStatus = {
  registry: { servers: { alpha: { command: "alpha-server", args: [], env: {}, headers: {} } } },
  auth: { alpha: { authorized: false } },
  active: ["alpha"],
  connections: [],
} as unknown as McpStatus;

function session(id: string, over: Partial<SessionRow> = {}): SessionRow {
  return {
    id,
    title: id,
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    workspace: "/src/bough",
    originDir: "/src/bough",
    busy: false,
    ...over,
  } as SessionRow;
}

/** A store that is a value, not a service: no socket, no fetch, no timers. */
function fakeStore(over: Partial<TuiState> = {}): Store & { calls: string[] } {
  const calls: string[] = [];
  const state: TuiState = { ...initialState(), ...over };
  const noop = () => Promise.resolve();
  const track = (name: string) => () => (calls.push(name), Promise.resolve());
  return {
    calls,
    getState: () => state,
    subscribe: () => () => {},
    dispatch: () => {},
    start: () => {},
    stop: noop,
    reload: noop,
    open: (id: string) => (calls.push(`open:${id}`), Promise.resolve()),
    createSession: () => Promise.resolve(null),
    send: noop,
    drainQueue: noop,
    answerAsk: noop,
    declineAsk: noop,
    refreshChanges: track("refreshChanges"),
    refreshJobs: track("refreshJobs"),
    refreshWorkflows: track("refreshWorkflows"),
    refreshReplay: (id: string) => (calls.push(`replay:${id}`), Promise.resolve()),
    resync: noop,
    notify: () => {},
    dismissNotice: () => {},
  };
}

const STATE: Partial<TuiState> = {
  connected: true,
  currentId: "s1",
  sessions: [session("s1", { title: "wire the panel" })],
  session: session("s1", { title: "wire the panel" }),
  changes: {
    available: false,
    reason: "this workspace is not a git repository",
    base: null,
    files: [],
    workspace: "/src/bough",
  },
};

function app(store: Store, over: Record<string, unknown> = {}) {
  return (
    <App
      store={store}
      models={MODELS}
      now={() => 10_000}
      controls={{ loadMcp: () => Promise.resolve(MCP) }}
      {...over}
    />
  );
}

// ---------------------------------------------------------------------------
// 2. A chord opens the panel, and every tab spec §15 names paints
// ---------------------------------------------------------------------------

Deno.test({
  name: "^t opens the one panel, and it is not open before that",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    const h = await mount(app(fakeStore(STATE)));
    try {
      await h.press("x"); // ordinary typing: the composer takes it, no panel
      assert.equal(h.frame().includes("^t close"), false, h.frame());
      await h.press(ctrl("t"));
      const open = h.frame();
      for (const tab of TABS) assert.ok(open.includes(tab.title), `${tab.title}: ${open}`);
      assert.ok(open.includes("^t close"), open);
      // …and ^t again closes it, back to the composer.
      await h.press(ctrl("t"));
      assert.equal(h.frame().includes("^t close"), false, h.frame());
    } finally {
      h.unmount();
    }
  },
});

/**
 * What each tab must have painted for us to believe it is the one showing.
 *
 * Each string is unique to its tab on purpose: "wire the panel" appears on both the
 * sessions list and the tree, so neither could use it as evidence of which is up.
 */
const EVIDENCE: Record<string, string> = {
  sessions: "/ filter",
  tree: "drill into delegated work",
  changes: "not a git repository",
  workflows: "no workflow runs in this conversation",
  model: "frontier model",
  mcp: "needs auth",
  skills: "skill discovery is not built yet",
  theme: "leaving the tab reverts",
};

/** Raw bytes a terminal sends for the keys ink has no letter for. */
const ESC = String.fromCharCode(27);
const SHIFT_TAB = ESC + "[Z";
const DOWN = ESC + "[B";

Deno.test({
  name: "every tab spec §15 names is selected by its own direct-jump chord",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    const store = fakeStore(STATE);
    const h = await mount(app(store));
    try {
      for (const tab of TABS) {
        // Straight from chat, with the panel CLOSED: that is what a direct jump means.
        const letter = tab.chord.replace("ctrl+", "");
        await h.press(ctrl(letter));
        const frame = h.frame();
        assert.ok(frame.includes("^t close"), `^${letter} did not open the panel: ${frame}`);
        assert.ok(
          frame.includes(EVIDENCE[tab.id]),
          `^${letter} did not show the ${tab.id} tab: ${frame}`,
        );
        await h.press(ESC); // back to chat, ready for the next jump
        assert.equal(h.frame().includes("^t close"), false, `esc left ${tab.id} open`);
      }
      // Entering a tab is what refreshes it — nothing is painted from a cache.
      assert.ok(store.calls.includes("refreshChanges"), store.calls.join(","));
      assert.ok(store.calls.includes("refreshWorkflows"), store.calls.join(","));
    } finally {
      h.unmount();
    }
  },
});

Deno.test({
  name: "⇥ cycles the tab bar, and ⏎ on a session opens it and closes the panel",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    const store = fakeStore(STATE);
    const h = await mount(app(store));
    try {
      await h.press(ctrl("t"));
      await h.press("\t");
      assert.ok(h.frame().includes(EVIDENCE.tree), h.frame()); // sessions → tree
      await h.press(SHIFT_TAB); // shift-tab, back to sessions
      assert.ok(h.frame().includes(EVIDENCE.sessions), h.frame());
      await h.press("\r");
      assert.ok(store.calls.includes("open:s1"), store.calls.join(","));
      assert.equal(h.frame().includes("^t close"), false, h.frame());
    } finally {
      h.unmount();
    }
  },
});

Deno.test({
  name: "a draft keeps the composer's chords, and never blocks ^t",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    const h = await mount(app(fakeStore(STATE)));
    try {
      await h.press("abc");
      // ^f is the tree's chord on an empty draft and the composer's forward-char
      // with text in it. With a draft, it must NOT open the panel.
      await h.press(ctrl("f"));
      assert.equal(h.frame().includes("^t close"), false, h.frame());
      // ^t collides with nothing, so a half-written message never hides the panel.
      await h.press(ctrl("t"));
      assert.ok(h.frame().includes("^t close"), h.frame());
    } finally {
      h.unmount();
    }
  },
});

Deno.test({
  name: "the theme tab repaints as the cursor moves, and reverts when it is left",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    const h = await mount(app(fakeStore(STATE)));
    try {
      await h.press(ctrl("y"));
      assert.ok(h.frame().includes("current: Default"), h.frame());
      // ↓ previews live. The preview object is mutable and lives OUTSIDE React, so
      // this is the assertion that catches a moved palette with a frozen cursor —
      // the defect the first live run of this panel actually had.
      await h.press(DOWN);
      const moved = h.frame();
      assert.ok(moved.includes("previewing Fjord"), moved);
      await h.press(DOWN);
      assert.ok(h.frame().includes("previewing Iris"), h.frame());
      // Leaving without ⏎ reverts: spec §16, browsing never commits.
      await h.press(ESC);
      await h.press(ctrl("y"));
      assert.ok(h.frame().includes("current: Default"), h.frame());
    } finally {
      h.unmount();
    }
  },
});

Deno.test({
  name: "with no conversation open the changes tab says so, and never spins",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    // No `currentId`: `store.refreshChanges()` fetches nothing, so a tab that trusted
    // `state.changes === null` to mean "in flight" would sit on "loading" forever.
    const h = await mount(app(fakeStore({ connected: true })));
    try {
      await h.press(ctrl("d"));
      const frame = h.frame();
      assert.ok(frame.includes("no conversation is open"), frame);
      assert.equal(frame.includes("loading changes"), false, frame);
    } finally {
      h.unmount();
    }
  },
});
