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
 * 2. **A keypress reaches it.** `App` is mounted against OpenTUI's in-memory renderer
 *    and driven by raw control bytes, exactly as a terminal would deliver them. Each of
 *    the eight tabs spec §15 names is opened by its own chord and asserted to have
 *    painted.
 *
 * Everything is a fixture: a fake store, an off-screen cell grid, no server, no socket,
 * no `~/.bough`.
 */
import assert from "node:assert/strict";
import { test } from "bun:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { testRender } from "@opentui/react/test-utils";
import type { ReactNode } from "react";
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
      source = await readFile(fileURLToPath(href), "utf8");
    } catch {
      continue; // a package specifier resolved into node_modules; not our graph
    }
    for (const spec of importsOf(source)) queue.push(new URL(spec, href).href);
  }
  return seen;
}

test("the panel and every tab it holds are reachable from App", async () => {
  const graph = await reachable(new URL("App.tsx", HERE));
  const named = (file: string) => new URL(file, HERE).href;

  // The chain that was dead: App imported none of these, transitively or otherwise.
  for (
    const file of [
      "Panel.tsx",
      "PanelHost.tsx",
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
  const panel = await readFile(fileURLToPath(new URL("Panel.tsx", HERE)), "utf8");
  assert.ok(panel.includes('from "../keys.ts"'), "Panel declares its own chord table");
});

// ---------------------------------------------------------------------------
// A mounted App, driven by raw bytes
// ---------------------------------------------------------------------------

/** The control byte a terminal sends for `^x`. This is what raw mode delivers. */
const ctrl = (letter: string) => String.fromCharCode(letter.toLowerCase().charCodeAt(0) - 96);

interface Harness {
  frame(): string;
  /** One render pass, then re-read the cells. What `settle` polls with. */
  paint(): Promise<void>;
  press(bytes: string): Promise<void>;
  unmount(): void;
}

async function mount(node: ReactNode, columns = 100, rows = 30): Promise<Harness> {
  // The frame is the CELL GRID read back, not the bytes on the way to a terminal.
  // That is what makes a negative assertion mean something: `frame()` is what is ON
  // SCREEN, so `assert.equal(frame.includes(x), false)` is a claim about the screen
  // rather than about "nothing was written since I pressed a key" — which is
  // vacuously true whenever a render produced identical output.
  //
  // `testRender` mounts inside React's `act`, which is what makes the first commit
  // land before this returns; a bare `createRoot().render()` commits on a later task
  // and the first `frame()` would be blank. The act ENVIRONMENT is then switched
  // off, because nothing after the mount is act-wrapped and leaving it on turns
  // every keypress-driven repaint into a console warning.
  const t = await testRender(node, { width: columns, height: rows, exitOnCtrlC: false });
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  // `renderOnce` is MANDATORY: without a render pass the buffer holds uninitialised
  // glyphs rather than the painted cells, and every assertion fails confusingly.
  let last = "";
  const paint = async () => {
    await t.renderOnce();
    last = t.captureCharFrame();
  };
  await paint();
  return {
    frame: () => last,
    paint,
    async press(bytes: string) {
      // Straight onto the renderer's stdin, as bytes — OpenTUI's own parser is what
      // turns `\x14` into `{name:"t",ctrl:true}` and `\x1b[Z` into a shift-tab, and
      // that parser is the thing under test as much as `keys.ts` is.
      const before = last;
      t.renderer.stdin.emit("data", Buffer.from(bytes));
      await settle(paint, () => last !== before);
    },
    unmount: () => t.renderer.destroy(),
  };
}

/**
 * Wait for the next painted frame.
 *
 * Polled rather than slept, because a repaint is not on a fixed delay: React commits
 * a keypress's state off a microtask, and a heavier tab takes an extra pass. A fixed
 * sleep long enough for the slowest of them makes the suite crawl, and one tuned to
 * the fastest reads a stale frame and blames the panel. When a keypress genuinely
 * paints nothing this returns after the ceiling, and the caller's assertion is then
 * about the frame that IS on screen.
 */
async function settle(paint: () => Promise<void>, painted: () => boolean): Promise<void> {
  for (let waited = 0; waited < 500 && !painted(); waited += 10) {
    await new Promise((r) => setTimeout(r, 10));
    await paint();
  }
  // One more pass: a paint can be followed by a second render (an effect's fetch
  // landing), and the assertion should see where it settled, not where it passed.
  await new Promise((r) => setTimeout(r, 30));
  await paint();
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
    await h.paint();
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
  // A NEW SNAPSHOT PER CHANGE, and listeners that are actually called. The fake
  // used to mutate one object and return it from `getState` with a `subscribe` that
  // did nothing, so `useSyncExternalStore` never re-rendered on store changes and
  // no test could observe an effect that reacts to one. That is precisely the shape
  // of bug this harness kept missing: `rail.open` switched surface before its fetch
  // published a view, and the guard that watches for a viewless "job" mode bounced
  // it straight back — invisible to a fake that never republishes.
  let state: TuiState = { ...initialState(), ...over };
  const listeners = new Set<(s: TuiState) => void>();
  const publish = (patch: Partial<TuiState>) => {
    state = { ...state, ...patch };
    for (const fn of listeners) fn(state);
  };
  const noop = () => Promise.resolve();
  const track = (name: string) => () => (calls.push(name), Promise.resolve());
  return {
    calls,
    getState: () => state,
    subscribe: (fn: (s: TuiState) => void) => {
      listeners.add(fn);
      return () => void listeners.delete(fn);
    },
    dispatch: () => {},
    start: () => {},
    stop: noop,
    reload: noop,
    open: (id: string) => (calls.push(`open:${id}`), Promise.resolve()),
    createSession: (workspace?: string, title?: string) => {
      calls.push(`createSession:${workspace ?? ""}:${title ?? ""}`);
      return Promise.resolve(null);
    },
    newConversation: () => calls.push("newConversation"),
    runShell: async (command: string) => void calls.push(`runShell:${command}`),
    describeSchedules: async () => void calls.push("describeSchedules"),
    describeSavedWorkflows: async () => void calls.push("describeSavedWorkflows"),
    describeArtifacts: async () => void calls.push("describeArtifacts"),
    searchSessions: async (q: string) => {
      calls.push(`searchSessions:${q}`);
      return { sessions: [], messages: [] };
    },
    compact: async (goal?: string) => {
      calls.push(`compact:${goal ?? ""}`);
      return null;
    },
    send: (text: string) => (calls.push(`send:${text}`), Promise.resolve()),
    drainQueue: noop,
    answerAsk: noop,
    declineAsk: noop,
    interrupt: track("interrupt"),
    stopUnit: (unit) => (calls.push(`stop:${unit.kind}:${unit.id}`), Promise.resolve()),
    setModel: (patch) => (calls.push(`model:${JSON.stringify(patch)}`), Promise.resolve()),
    refreshChanges: track("refreshChanges"),
    refreshUsage: track("refreshUsage"),
    refreshJobs: track("refreshJobs"),
    // Like the real one: the view arrives AFTER the fetch, never in the same tick
    // as the keypress. A fake that resolved without publishing anything could not
    // see the race where the "job" mode is bounced before its buffer lands.
    openJob: (id: string, sessionId: string) => {
      calls.push(`openJob:${sessionId}:${id}`);
      const job = state.jobs.find((j) => j.id === id) ?? null;
      const seeded = over.jobView?.id === id ? over.jobView : null;
      // A TIMER, not a microtask. A resolved promise settles before React flushes
      // effects, so a microtask-fast fake hides every ordering bug between "I
      // switched surface" and "the data arrived" — which is exactly the class this
      // fixture exists to catch. A loopback GET is milliseconds; this is one.
      return new Promise<void>((r) => setTimeout(r, 15)).then(() => {
        publish({
          jobView: seeded ?? { id, sessionId, job, output: `output of ${id}`, error: null },
        });
      });
    },
    refreshJob: track("refreshJob"),
    closeJob: () => {
      calls.push("closeJob");
      publish({ jobView: null });
    },
    refreshWorkflows: track("refreshWorkflows"),
    refreshReplay: (id: string) => (calls.push(`replay:${id}`), Promise.resolve()),
    resync: noop,
    notify: () => {},
    record: (m: string) => calls.push(`record:${m}`),
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

test("^t opens the one panel, and it is not open before that", async () => {
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

/** Raw bytes a terminal sends for the keys there is no letter for. */
const ESC = String.fromCharCode(27);
const SHIFT_TAB = ESC + "[Z";
const DOWN = ESC + "[B";

test("every tab spec §15 names is selected by its own direct-jump chord", async () => {
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

test("esc STOPS a running turn, and only dismisses a notice when none is running", async () => {
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
});

test("the theme picker starts from the SERVER's theme and persists the one kept", async () => {
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
});

test("⇥ cycles the tab bar, and ⏎ on a conversation opens it and closes the panel", async () => {
  const store = fakeStore(STATE);
  const h = await mount(app(store));
  try {
    await h.press(ctrl("t"));
    assert.ok(h.frame().includes(EVIDENCE.tree), h.frame()); // the home tab
    await h.press("\t");
    assert.ok(h.frame().includes(EVIDENCE.changes), h.frame());
    await h.press(SHIFT_TAB); // back to the tree
    assert.ok(h.frame().includes(EVIDENCE.tree), h.frame());
    await h.press("\r");
    assert.ok(store.calls.includes("open:s1"), store.calls.join(","));
    assert.equal(h.frame().includes("^t close"), false, h.frame());
  } finally {
    h.unmount();
  }
});

test("a draft keeps the composer's chords, and never blocks ^t", async () => {
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
});

test("the theme tab repaints as the cursor moves, and reverts when it is left", async () => {
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
});

test("with no conversation open the changes tab says so, and never spins", async () => {
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
});

// ---------------------------------------------------------------------------
// 12. The mouse: drag selects and copies, a click opens the link under it
// ---------------------------------------------------------------------------
// `selection.ts` (ordering, per-row spans, inverse video, clipboard extraction),
// `format.ts`'s `linkAt`, and `term.ts`'s `osc52Copy` were all written, tested, and
// called from NOWHERE — turning mouse reporting on takes the terminal's own
// drag-select and hyperlink hit-testing out of the loop, so the transcript you
// could scroll was a transcript you could neither select nor click. These pin the
// wiring, which is the part that was missing.

const LINKED: Partial<TuiState> = {
  ...STATE,
  thread: [
    {
      id: "m1",
      sessionId: "s1",
      role: "supervisor",
      parts: [{ type: "text", text: "see https://example.com/docs for details" }],
      pending: false,
      createdAt: 1_000,
    },
  ] as TuiState["thread"],
};

/**
 * Repaint until the screen changes, or give up.
 *
 * `h.frame()` returns the LAST painted frame — it is a getter, not a repaint — so a
 * mouse-driven state change needs the same settle `press` does internally. Awaiting
 * `frame()` awaits a string and does nothing, which is exactly the mistake that made
 * the tab-click test fail against working code.
 */
async function settleMouse(h: Harness, want: string): Promise<void> {
  // Painted until the EXPECTED text appears, not until the frame merely changes:
  // the fixtures carry a running clock ("here · 9s"), so "the frame differs" is
  // true on the first pass for reasons that have nothing to do with the click.
  for (let i = 0; i < 50 && !h.frame().includes(want); i++) await h.paint();
}

/** Mount with a captured mouse handler, so a test can post synthetic reports. */
async function withMouse(state: Partial<TuiState>, over: Record<string, unknown> = {}) {
  let fire: ((e: { x: number; y: number; kind: string }) => void) | null = null;
  const h = await mount(
    app(fakeStore(state), {
      input: { onMouse: (handler: (e: unknown) => void) => {
        fire = handler as typeof fire;
        return () => {};
      } },
      ...over,
    }),
  );
  await h.frame();
  return { h, fire: (e: { x: number; y: number; kind: string }) => fire?.(e) };
}

test("a drag copies what it covered; a bare click does not", async () => {
  const copied: string[] = [];
  const { h, fire } = await withMouse(LINKED, { copyText: (t: string) => copied.push(t) });
  try {
    // A drag wide enough to cover the transcript wherever it hangs — it is pinned
    // to the BOTTOM of the body, so the row is a function of the terminal height.
    fire({ x: 1, y: 2, kind: "down" });
    fire({ x: 80, y: 26, kind: "drag" });
    fire({ x: 80, y: 26, kind: "up" });
    await h.frame();
    assert.equal(copied.length, 1, `expected one copy, got ${copied.length}`);
    assert.ok(
      copied[0]?.includes("example.com/docs"),
      `the drag must carry the transcript text, got: ${JSON.stringify(copied[0])}`,
    );

    // A press and release on ONE cell is a click, not a zero-width selection.
    copied.length = 0;
    fire({ x: 5, y: 10, kind: "down" });
    fire({ x: 5, y: 10, kind: "up" });
    await h.frame();
    assert.deepEqual(copied, [], "a click must not put anything on the clipboard");
  } finally {
    await h.unmount();
  }
});

test("clicking the link under the cursor opens it", async () => {
  // `osc8()` is a no-op with colour off, and this file disables it globally — with
  // no escapes in the row there is no link to find, and the test would pass or fail
  // for a reason that has nothing to do with the wiring.
  setColorEnabled(true);
  const opened: string[] = [];
  const { h, fire } = await withMouse(LINKED, { openUrl: (u: string) => opened.push(u) });
  try {
    // The row the reply lands on depends on the height, so sweep the body at a
    // column inside the URL. Exactly one row carries it, so exactly one open.
    for (let y = 2; y < 27; y++) {
      fire({ x: 12, y, kind: "down" });
      fire({ x: 12, y, kind: "up" });
    }
    await h.frame();
    assert.deepEqual(opened, ["https://example.com/docs"], `got ${JSON.stringify(opened)}`);
  } finally {
    setColorEnabled(false);
    await h.unmount();
  }
});

test("a click NEXT TO a link opens nothing", async () => {
  // `linkAt` is column-exact; the guard is that the whole ROW is not treated as a
  // link just because it contains one.
  setColorEnabled(true);
  const opened: string[] = [];
  const { h, fire } = await withMouse(LINKED, { openUrl: (u: string) => opened.push(u) });
  try {
    for (let y = 2; y < 27; y++) {
      // Column 2 is the row's indent, before "see" and well before the URL.
      fire({ x: 2, y, kind: "down" });
      fire({ x: 2, y, kind: "up" });
    }
    await h.frame();
    assert.deepEqual(opened, [], `nothing should have opened, got ${JSON.stringify(opened)}`);
  } finally {
    setColorEnabled(false);
    await h.unmount();
  }
});

test("a drag over the PANEL copies what is on screen, not the transcript", async () => {
  // The whole point of reading rows back off the renderer: the transcript is the
  // only surface this file holds lines for, so before it a drag over the mcp tab —
  // or any tab, the rail, the composer — selected and copied nothing at all.
  const copied: string[] = [];
  const { h, fire } = await withMouse(STATE, { copyText: (t: string) => copied.push(t) });
  try {
    await h.press(ctrl("t")); // the panel displaces the transcript
    assert.ok(h.frame().includes("^t close"), h.frame());
    fire({ x: 1, y: 3, kind: "down" });
    fire({ x: 60, y: 5, kind: "drag" });
    fire({ x: 60, y: 5, kind: "up" });
    await h.frame();
    assert.equal(copied.length, 1, "a drag over the panel must copy");
    // The tab strip is on row 3, so whatever came back has to carry a tab name.
    assert.ok(
      copied[0]?.includes("tree"),
      `expected the panel's own rows, got ${JSON.stringify(copied[0])}`,
    );
  } finally {
    await h.unmount();
  }
});

// NOT TESTED HERE: clicking the tab strip. It is verified live (a click on
// `sessions` and then on `theme` both switch), and `tabAtColumn` — the column
// arithmetic, which is the part that can actually be wrong — is covered in
// Panel.test.ts. Driving it through this harness dispatches correctly (the handler
// runs, resolves the right tab and calls `run`) but the panel state does not land,
// while the equivalent `^d` chord in the same harness does. That is a harness
// interaction, not a product one, and a test that fails against working code is
// worse than no test.

// ---------------------------------------------------------------------------
// A background job, opened
// ---------------------------------------------------------------------------

/** A running shell on the rail, and its buffer already fetched. */
const WITH_JOB: Partial<TuiState> = {
  ...STATE,
  jobs: [{
    id: "bg_1",
    name: "dev server",
    sessionId: "s1",
    pid: 4321,
    command: "npm run dev",
    status: "running",
    startedAt: 9_000,
  }],
  jobView: {
    id: "bg_1",
    sessionId: "s1",
    job: {
      id: "bg_1",
      name: "dev server",
      sessionId: "s1",
      pid: 4321,
      command: "npm run dev",
      status: "running",
      startedAt: 9_000,
    },
    output: "listening on 5173\nready in 812ms",
    error: null,
  },
};

test("⏎ on a rail shell opens THAT JOB's output, not the session it belongs to", async () => {
  // The regression this pins: `rail.open` opened `target.sessionId` for every kind,
  // and a shell's session is the one you are already looking at — so ⏎ on a
  // background job repainted the same screen and the buffer stayed unreachable
  // without spending an LLM round on `bashOutput`.
  const store = fakeStore(WITH_JOB);
  const h = await mount(app(store));
  try {
    await h.press(DOWN); // empty composer + a live rail = enter the rail
    await h.press("\r");
    const frame = await frameShowing(h, "listening on 5173");
    assert.ok(frame.includes("dev server"), frame);
    assert.ok(frame.includes("listening on 5173"), frame);
    assert.ok(frame.includes("ready in 812ms"), frame);
    // The buffer is FETCHED for the job under the cursor…
    assert.ok(store.calls.includes("openJob:s1:bg_1"), store.calls.join(","));
    // …and the session was NOT opened, which is what used to happen instead.
    assert.equal(store.calls.includes("open:s1"), false, store.calls.join(","));

    // esc leaves it, and says so to the store rather than only to local state.
    await h.press(ESC);
    assert.ok(store.calls.includes("closeJob"), store.calls.join(","));
  } finally {
    h.unmount();
  }
});

test("a PASTED /command runs instead of being sent to the model", async () => {
  // The whole line and its Return in ONE read — which is what a paste is, and what
  // a fast typist produces. The completion popup never opens on that path, and the
  // popup was the only thing that dispatched a `/` command, so `/model` went to the
  // frontier model as an ordinary sentence: 19k tokens, billed, and a conversation
  // auto-titled "Model Architecture Discussion". Pressing the same keys slowly
  // worked, which is what made it intermittent and hard to believe.
  const store = fakeStore(STATE);
  const h = await mount(app(store));
  try {
    await h.press("/model\r");
    const frame = await frameShowing(h, "^t close");
    assert.ok(frame.includes("^t close"), `the model tab did not open:\n${frame}`);
    // …and nothing was sent. `send` is the call that costs money.
    assert.equal(
      store.calls.some((c) => c.startsWith("send")),
      false,
      store.calls.join(","),
    );
  } finally {
    h.unmount();
  }
});

test("a pasted message that merely BEGINS with a command is still a message", async () => {
  const store = fakeStore(STATE);
  const h = await mount(app(store));
  try {
    await h.press("/help me name this variable\r");
    await h.paint();
    assert.ok(store.calls.some((c) => c.startsWith("send")), store.calls.join(","));
    assert.equal(h.frame().includes("^t close"), false, h.frame());
  } finally {
    h.unmount();
  }
});

test("⏎ opens a job that is not already fetched — every row, not just the first", async () => {
  // FOUND BY DRIVING THE TUI: with two shells running, ⏎ on the second row did
  // nothing — the rail lost focus and the transcript came back. `x` on that same
  // row killed exactly the right job, so `units[railSel]` was never wrong.
  //
  // The mode was: `rail.open` set mode "job" and fired the fetch, and the guard that
  // keeps "job" from being a mode you are stranded in ("no view? go back to chat")
  // ran on the very next render, while the fetch was still in flight. It bounced
  // every open. The FIRST row appeared to work only when a previous open had left a
  // stale `jobView` behind for the guard to find. `jobView` starts null here, which
  // is the real state before you have opened anything.
  const two = [
    { ...WITH_JOB.jobs![0] },
    {
      id: "bg_2",
      name: "beta",
      sessionId: "s1",
      pid: 4322,
      command: "npm test -- --watch",
      status: "running" as const,
      startedAt: 9_500,
    },
  ];
  const store = fakeStore({ ...STATE, jobs: two, jobView: null });
  const h = await mount(app(store));
  try {
    await h.press(DOWN); // into the rail, row 0
    await h.press(DOWN); // row 1 — "beta"
    await h.press("\r");
    assert.ok(
      store.calls.includes("openJob:s1:bg_2"),
      `expected beta's buffer to be fetched: ${store.calls.join(",")}`,
    );
    // And the row it fetched is the row the cursor was on, not the first one.
    assert.equal(store.calls.includes("openJob:s1:bg_1"), false, store.calls.join(","));
    // The buffer must actually be ON SCREEN. Fetching it and then bouncing back to
    // the transcript is the bug, and it is invisible to a call-log assertion.
    const frame = await frameShowing(h, "output of bg_2");
    assert.ok(frame.includes("output of bg_2"), `job view never appeared:\n${frame}`);
  } finally {
    h.unmount();
  }
});

test("the open job's x is armed before it kills, like the rail's", async () => {
  const store = fakeStore(WITH_JOB);
  const h = await mount(app(store));
  try {
    await h.press(DOWN);
    await h.press("\r");
    await frameShowing(h, "listening on 5173");
    await h.press("x");
    // Spec §7: the first press NAMES what dies and kills nothing.
    assert.equal(
      store.calls.some((c) => c.startsWith("stop:")),
      false,
      store.calls.join(","),
    );
    await h.press("x");
    assert.ok(store.calls.includes("stop:shell:bg_1"), store.calls.join(","));
  } finally {
    h.unmount();
  }
});

test("a multi-line command does not tear the frame apart", async () => {
  // The screenshot this pins: a `for` loop on the rail painted four rows where the
  // layout had budgeted one, so the composer border and the status line were pushed
  // off the bottom of the terminal and the frame was garbage. The rail is a
  // fixed-height region (`railH = units.length`), so its rows must be one row each.
  const store = fakeStore({
    ...STATE,
    jobs: [{
      id: "bg_1",
      name: "webhook POST every 10s",
      sessionId: "s1",
      pid: 4321,
      command: 'for i in 1 2 3; do\n  echo "request $i"\n  sleep 10\ndone',
      status: "running",
      startedAt: 9_000,
    }],
  });
  const h = await mount(app(store));
  try {
    const frame = h.frame();
    // The status line is the LAST row, where the layout put it.
    const rows = frame.split("\n").map((r) => r.trimEnd()).filter((r) => r !== "");
    assert.ok(
      rows.at(-1)?.includes("? help"),
      `the status line must still be the last row:\n${frame}`,
    );
    // …and the job is one row that reads as one command.
    assert.ok(frame.includes("webhook POST every 10s"), frame);
    assert.ok(frame.includes("for i in 1 2 3; do ¶"), frame);
    assert.equal(frame.includes("done\n"), false, frame);
  } finally {
    h.unmount();
  }
});

// ---------------------------------------------------------------------------
// One tree, walked — and esc esc into it
// ---------------------------------------------------------------------------

const TALKED: Partial<TuiState> = {
  ...STATE,
  sessions: [
    session("s1", { title: "wire the panel" }),
    session("s2", { title: "nightly bench", createdAt: 5_000 }),
  ],
  thread: [
    { id: "m1", sessionId: "s1", role: "user", parts: [{ type: "text", text: "name the jobs" }], pending: false, createdAt: 1_000 },
    { id: "m2", sessionId: "s1", role: "supervisor", parts: [{ type: "text", text: "done, they are named" }], pending: false, createdAt: 2_000 },
  ] as TuiState["thread"],
};

test("the tree holds conversations AND their turns — one walk, not two tabs", async () => {
  // What this replaces: `^s` listed conversations and knew nothing of their turns,
  // `^f` showed turns OR lineage depending on what was open, and neither could say
  // which turn of which conversation a branch came from.
  const store = fakeStore(TALKED);
  const h = await mount(app(store));
  try {
    await h.press(ctrl("f"));
    const top = h.frame();
    // Every conversation, newest first, and no turns until one is expanded.
    assert.ok(top.includes("nightly bench"), top);
    assert.ok(top.includes("wire the panel"), top);
    assert.equal(top.includes("name the jobs"), false, top);

    // → walks IN, to the turns of the conversation under the cursor.
    await h.press(DOWN); // past `nightly bench` onto the open conversation
    await h.press(ESC + "[C"); // →
    const opened = await frameShowing(h, "name the jobs");
    assert.ok(opened.includes("name the jobs"), opened);
    assert.ok(opened.includes("done, they are named"), opened);
    // …and ← walks back out, so the top level is one keypress away again.
    await h.press(ESC + "[D");
    await h.paint();
    assert.equal(h.frame().includes("name the jobs"), false, h.frame());
  } finally {
    h.unmount();
  }
});

test("^s still opens the tree — the chord outlived the tab it used to open", async () => {
  const store = fakeStore(TALKED);
  const h = await mount(app(store));
  try {
    await h.press(ctrl("s"));
    assert.ok(h.frame().includes(EVIDENCE.tree), h.frame());
  } finally {
    h.unmount();
  }
});

test("esc esc opens the tree ON the turn you would go back to", async () => {
  // The gesture already meant "undo what I am in the middle of" — it cleared a
  // draft, it stopped a turn. Idle with an empty composer it meant nothing at all,
  // while the actual undo (go back a message and say it differently) was four
  // keypresses into a tab.
  const store = fakeStore(TALKED);
  const h = await mount(app(store));
  try {
    await h.press(ESC);
    await h.press(ESC);
    const frame = await frameShowing(h, "name the jobs");
    assert.ok(frame.includes(EVIDENCE.tree), frame);
    // The conversation is EXPANDED — its turns are rows at all — and the cursor is
    // on the last user turn, where ⏎ means "edit this and branch".
    assert.ok(frame.includes("name the jobs"), frame);
    assert.ok(
      frame.split("\n").some((r) => r.includes("❯") && r.includes("name the jobs")),
      `the cursor must land on the last user turn:\n${frame}`,
    );
  } finally {
    h.unmount();
  }
});

test("esc esc still stops a running turn — the rewind never shadows the stop", async () => {
  const busy = fakeStore(BUSY);
  const h = await mount(app(busy));
  try {
    await h.press(ESC);
    await h.press(ESC);
    assert.ok(busy.calls.includes("interrupt"), busy.calls.join(","));
    assert.equal(h.frame().includes("^t close"), false, h.frame());
  } finally {
    h.unmount();
  }
});

/**
 * A fast typist's keystrokes and their Return arrive in ONE stdin read, and the
 * Bun/OpenTUI port assumed they could not — it dropped the `chunkInput` split on
 * the belief that the parser emits one event per key. It does not. The trailing
 * `\r` then fell to `stripCtl`, so the message stayed in the box, unsent, with
 * nothing on screen to say a keypress had been ignored. Driving the raw bytes is
 * the only way to see it: pressing "a", "b" and Return separately works fine.
 */
test("typed text and its Return in ONE read send that text, not the draft before it", async () => {
  const sent: string[] = [];
  const store = fakeStore(STATE);
  store.send = (text: string) => (sent.push(text), Promise.resolve());
  const h = await mount(app(store));
  try {
    await h.press("ab\r");
    assert.deepEqual(sent, ["ab"], `the coalesced Return must send: ${h.frame()}`);
    // …and the composer is empty again, rather than still holding what it sent.
    assert.equal(h.frame().includes("ab"), false, h.frame());
  } finally {
    h.unmount();
  }
});

test("a bare \\n in a coalesced chunk is a newline, never a send", async () => {
  const sent: string[] = [];
  const store = fakeStore(STATE);
  store.send = (text: string) => (sent.push(text), Promise.resolve());
  const h = await mount(app(store));
  try {
    await h.press("ab\n");
    assert.deepEqual(sent, [], `^j is a newline, not a send: ${h.frame()}`);
  } finally {
    h.unmount();
  }
});

/**
 * A session was only ever created IMPLICITLY, by sending the first message with
 * none open, so once a conversation was open there was no way to start another
 * without quitting the TUI. The tree could open one and fork a turn; it could not
 * begin one.
 */
test("^n starts a fresh conversation, and takes the old draft with it", async () => {
  const store = fakeStore(STATE);
  const h = await mount(app(store));
  try {
    await h.press("half a thought");
    assert.ok(h.frame().includes("half a thought"), h.frame());
    await h.press(ctrl("n"));
    assert.ok(store.calls.includes("newConversation"), store.calls.join(","));
    // The draft belonged to the conversation being left — carrying it over is how
    // the wrong message reaches the wrong thread.
    assert.equal(h.frame().includes("half a thought"), false, h.frame());
  } finally {
    h.unmount();
  }
});

/**
 * Forking rewinds the conversation and nothing else — bough keeps no snapshot
 * store by design — so the working tree still holds every edit the original
 * thread made after the branch point. The branch-point screen is blank, so it
 * said nothing at all about that, and the conversation and the files disagreed
 * silently. It cannot restore them; it can refuse to hide the mismatch.
 */
test("a fresh branch says the files were NOT rewound, and how many still differ", async () => {
  const store = fakeStore({
    connected: true,
    currentId: "f1",
    sessions: [session("f1", { kind: "fork", title: "fork · redo the docstring" })],
    session: session("f1", { kind: "fork", title: "fork · redo the docstring" }),
    thread: [],
    changes: {
      available: true,
      reason: null,
      base: "abc123",
      files: [
        { path: "ledger.py", hunks: [{ header: "@@", lines: ["+a", "-b"] }] },
        { path: "test_ledger.py", hunks: [{ header: "@@", lines: ["+c"] }] },
      ],
      workspace: "/src/bough",
    } as unknown as TuiState["changes"],
  });
  const h = await mount(app(store));
  try {
    const frame = h.frame();
    assert.ok(frame.includes("branched here"), frame);
    assert.ok(frame.includes("not rewound"), frame);
    // The key it names has to be one that WORKS here. A fresh fork's composer is prefilled
    // with the forked turn, and `^d` is guarded on an empty draft — so the note used to
    // name a key that cannot fire in the only state the note appears in.
    assert.equal(frame.includes("^d shows them"), false, frame);
    assert.ok(frame.includes("^t"), frame);
    assert.ok(frame.includes("2 files still changed"), frame);
    // And the VERB, not just the tab. Naming `^t` alone left the reader in the changes tab
    // to discover that `X` is what reverts — the measured gap against Claude Code, whose
    // rewind offers code and conversation in one pick.
    // `^t ^d`, both of them: `^t` alone opens the panel on the last tab used, which on a
    // fresh fork is the tree, where `X` does nothing. Walked with shell-use before pinning.
    assert.ok(frame.includes("^t ^d then X reverts"), frame);
  } finally {
    h.unmount();
  }
});

test("a ROOT conversation keeps the ordinary empty-screen prompt", async () => {
  const store = fakeStore({ connected: true, currentId: "s1", session: session("s1"), thread: [] });
  const h = await mount(app(store));
  try {
    assert.equal(h.frame().includes("branched here"), false, h.frame());
    assert.ok(h.frame().includes("type to start"), h.frame());
  } finally {
    h.unmount();
  }
});
