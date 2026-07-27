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
import type { SkillRow } from "./Skills.tsx";

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

/**
 * The frame once it shows `needle` — or, at the ceiling, whatever is on screen.
 *
 * A tab whose body arrives from an injected thunk paints TWICE: once with the panel
 * open and the body still `null` ("loading…"), then again when the promise resolves.
 * `press` returns on the first of those, so the fixed grace period inside `settle`
 * is a bet that the second one lands within 30ms — and under a full-suite run, with
 * every test file competing for the same cores, that bet loses often enough to make
 * this the one flaky test in the tree.
 *
 * Polling for the evidence makes the wait proportional to the machine rather than to
 * a guess: a fast run costs nothing extra, and a loaded one waits as long as it must.
 * The ceiling still expires, so a tab that genuinely never paints fails with the
 * frame that IS on screen rather than hanging.
 */
async function frameShowing(h: Harness, needle: string): Promise<string> {
  for (let waited = 0; waited < 2_000 && !h.frame().includes(needle); waited += 10) {
    await new Promise((r) => setTimeout(r, 10));
  }
  return h.frame();
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

/** What `GET /skills` serves. One good skill and one broken one, because the tab
 * must distinguish them and a listing of only healthy rows would not prove it. */
const SKILLS: { skills: SkillRow[]; sources: { source: string; dir: string }[] } = {
  skills: [
    {
      name: "history",
      description: "query bough's own sqlite",
      source: "bundled",
      dir: "/b/history",
    },
    { name: "broken", description: "", source: "user", dir: "/u/broken", error: "no name:" },
  ],
  sources: [{ source: "bundled", dir: "/b" }, { source: "user", dir: "/u" }],
};

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
    interrupt: track("interrupt"),
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
      controls={{
        loadMcp: () => Promise.resolve(MCP),
        // T10.2 landed and the panel reads it. Injected here for the same reason
        // `loadMcp` is: this is transport, and no test binds a socket.
        loadSkills: () => Promise.resolve(SKILLS),
      }}
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
 * The bracketed marker in the tab strip, which is the one signal that is unique
 * per tab, always present, and legible with colour off. It used to be a phrase
 * scraped from each tab's body — and the sessions entry was "/ filter", a footer
 * advertising a key the keymap never bound, so the test was pinned to a lie.
 */
const EVIDENCE: Record<string, string> = Object.fromEntries(
  TABS.map((t) => [t.id, `[${t.id}]`]),
);

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
        const frame = await frameShowing(h, EVIDENCE[tab.id]);
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

// ---------------------------------------------------------------------------
// 2b. The user interrupt (spec §5), and the theme's two server-facing halves
// ---------------------------------------------------------------------------

/** A pending supervisor message is what `isBusy` reads — the turn is in flight. */
const BUSY: Partial<TuiState> = {
  ...STATE,
  thread: [
    {
      id: "m1",
      sessionId: "s1",
      role: "supervisor",
      parts: [],
      pending: true,
      createdAt: 1_000,
    },
  ] as TuiState["thread"],
};

Deno.test({
  name: "esc STOPS a running turn, and only dismisses a notice when none is running",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    // The gap this closes: `turn/runner.ts` has always been able to interrupt and
    // nothing in either client could reach it, so the only stop button was killing
    // the server. The guard is the binding — `keys.ts` routes esc to `turn.interrupt`
    // only while a turn is in flight — which is why both halves are asserted here.
    const busy = fakeStore(BUSY);
    const h = await mount(app(busy));
    try {
      await h.press(ESC);
      assert.ok(busy.calls.includes("interrupt"), busy.calls.join(","));
    } finally {
      h.unmount();
    }

    const idle = fakeStore(STATE);
    const h2 = await mount(app(idle));
    try {
      await h2.press(ESC);
      assert.equal(
        idle.calls.includes("interrupt"),
        false,
        `esc must not raise an interrupt with nothing running: ${idle.calls.join(",")}`,
      );
    } finally {
      h2.unmount();
    }
  },
});

Deno.test({
  name: "the theme picker starts from the SERVER's theme and persists the one kept",
  sanitizeOps: false,
  sanitizeResources: false,
  fn: async () => {
    // Two failures this covers, both of which shipped: with no `current` the picker's
    // baseline is "Default", so leaving the tab reverts a stored theme off the screen;
    // with no `persist` keeping one lasts until the process exits.
    const stored = { theme: { name: "Fjord", colors: { green: "#5c88c9" } }, defaults: {} };
    const saved: string[] = [];
    const store = fakeStore(STATE);
    const h = await mount(
      app(store, {
        theme: { current: stored, persist: (p: { name: string }) => saved.push(p.name) },
      }),
    );
    try {
      await h.press(ctrl("y"));
      // The cursor starts on the theme in force, not on row zero.
      assert.match(await frameShowing(h, "current: Fjord"), /current: Fjord/);
      await h.press(DOWN);
      await h.press("\r");
      assert.equal(saved.length, 1, `keeping a theme must write it through: ${saved.join(",")}`);
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
      // …and NOT the non-git sentence. "the agent still works here" is a claim about
      // a checkout, and with no session open there is no checkout to make it about.
      assert.equal(
        frame.includes("the agent still works here"),
        false,
        `the non-git hint must not be shown when there is no session at all: ${frame}`,
      );
    } finally {
      h.unmount();
    }
  },
});
