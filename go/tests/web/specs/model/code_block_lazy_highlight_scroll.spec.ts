// go/tests/model/specs/code_block_lazy_highlight_scroll.fizz in the
// browser (recipe: go/tests/model/README.md): the real `Code` component
// (go/internal/serve/web/src/code.tsx) never crashes on an unregistered
// language and always falls back to a plain `<pre>`, for two code blocks
// that can be at any of mount/visible/scrolled-away independently. The
// Go twin, a made-up model of a hypothetical virtualized transcript (the
// real transcript has no windowing), is
// go/tests/model/mbt/code_block_lazy_highlight_scroll_test.go.
//
// The role instance under test is CodeBlock#0: a single session whose
// `tools.view(path)` calls are held out of the page (queued and run
// while the page sits on the session list) until ScrollIntoView
// navigates to the session — that navigation is this walk's only stand-in
// for a virtualized transcript's mount, since the real transcript renders
// every row unconditionally. `rendered` is read fresh off the DOM each
// time (never cached at action time), off the last `.tool-output pre.hl`
// (highlight.js wraps a recognised token in a child <span>; the plain
// fallback has none). CodeBlock#1's actions are the walk's own
// bookkeeping only (mirroring the spec's transitions, like `pendingShow`/
// `pendingHide` in call_row_hover_popover_race.spec.ts): the graph
// interleaves both instances' actions, but only CodeBlock#0's state is
// checked (`role: 'CodeBlock#0'`), and CodeBlock#1 never touches the
// page.
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, waitTaken } from '../../helpers/control';
import { loadPaths, modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

type Lang = 'known' | 'unknown';
type Rendered = 'none' | 'highlighted' | 'plain';

// Which literal instance (CodeBlock#0 or #1) each step of THIS walk's
// trace names. Deterministic and identical to modelTests' own
// loadPaths(...) call: same pure function over the checked-in graph.
let nextWalk = 0;

interface Other { mounted: boolean; visible: boolean; lang: Lang; rendered: Rendered }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;
  n: number; // turn counter, unique across every tools.view() this walk queues
  mounted: boolean;
  visible: boolean;
  lang: Lang;
  onSession: boolean; // whether the page is currently navigated to the session
  other: Other; // CodeBlock#1's bookkeeping; never read by the framework
  qualified: string[]; // this walk's own qualified action names, e.g. "CodeBlock#1.MountKnown"
  cursor: number;
}

// The instance the NEXT performed action belongs to (advances the cursor
// exactly once per call, in lockstep with modelTests' own trace walk).
function instanceOf(c: Ctx): 0 | 1 {
  const q = c.qualified[c.cursor++];
  if (!q) throw new Error('code_block_lazy_highlight_scroll: walk ran past its recorded trace');
  return q.startsWith('CodeBlock#0.') ? 0 : 1;
}

async function untilDone(serve: Serve, id: string): Promise<void> {
  const deadline = Date.now() + 20_000;
  for (;;) {
    const res = await serve.api.get(`/api/sessions/${id}`);
    if (!res.ok()) throw new Error(`session status: ${res.status()} ${await res.text()}`);
    if ((await res.json()).session.status !== 'running') return;
    if (Date.now() > deadline) throw new Error('turn did not finish');
    await new Promise((r) => setTimeout(r, 50));
  }
}

const PATH: Record<Lang, string> = { known: 'known.go', unknown: 'unknown.rs' };
const BODY: Record<Lang, string> = { known: 'package main\n\nfunc main() {}\n', unknown: 'fn main() {}\n' };

// A held turn whose one call is `tools.view(path)`: sent while the page
// sits on the session list, so nothing about it reaches this page's DOM
// until a later ScrollIntoView navigates there.
async function send(c: Ctx, text: string): Promise<void> {
  c.n++;
  const name = `t${String(c.n).padStart(5, '0')}`;
  queue(c.dir, name, { mode: 'ok', text });
  const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: name } });
  if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
  await waitTaken(c.dir, name);
  await untilDone(c.serve, c.id);
}

// MountKnown/MountUnknown on the real instance: the file already exists
// (written once at Init), so mounting is reading it — the turn completes
// before the page ever shows it, matching "mounted but not visible".
async function mount(c: Ctx, lang: Lang): Promise<void> {
  await send(c, '```js\ntools.view(' + JSON.stringify(PATH[lang]) + ')\n```');
  c.mounted = true;
  c.visible = false;
  c.lang = lang;
}

// The only point that puts the block in this page's DOM at all.
async function scrollIntoView(c: Ctx): Promise<void> {
  if (!c.onSession) {
    await c.page.goto(`${c.serve.url}/#/s/${c.id}`);
    c.onSession = true;
  }
  await c.page.locator('.transcript details.toolcall').last().waitFor();
  c.visible = true;
}

// Navigating off the session removes every block from the DOM, exactly
// like a fresh one when it next scrolls in.
async function scrollAway(c: Ctx): Promise<void> {
  await c.page.goto(`${c.serve.url}/#/`);
  await c.page.locator('.sidebar').waitFor();
  c.onSession = false;
  c.mounted = false;
  c.visible = false;
}

// CodeBlock#1: never touches the page, only mirrors the spec's own
// transitions (unread by the framework — role: 'CodeBlock#0').
function mountOther(c: Ctx, lang: Lang): void {
  c.other = { mounted: true, visible: false, lang, rendered: 'none' };
}
function scrollIntoViewOther(c: Ctx): void {
  c.other.visible = true;
  c.other.rendered = c.other.lang === 'known' ? 'highlighted' : 'plain';
}
function scrollAwayOther(c: Ctx): void {
  c.other = { mounted: false, visible: false, lang: c.other.lang, rendered: 'none' };
}

// `rendered` is derived fresh off the DOM every read, never cached at
// action time: a still-loading transcript just fails one poll and is
// tried again, instead of baking a race into the walk.
async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  let rendered: Rendered = 'none';
  if (c.visible) {
    rendered = (await c.page.evaluate(() => {
      const nodes = document.querySelectorAll('.transcript details.toolcall .tool-output pre.hl');
      const el = nodes[nodes.length - 1] as HTMLElement | undefined;
      if (!el) return 'none';
      return el.children.length > 0 ? 'highlighted' : 'plain';
    })) as Rendered;
  }
  return { mounted: c.mounted, visible: c.visible, lang: c.lang, rendered };
}

modelTests<Ctx>({
  spec: 'code_block_lazy_highlight_scroll',
  role: 'CodeBlock#0',
  config: CONTROL_CONFIG + '- id: loop\n  plugin: loop\n',

  async init(page, serve) {
    const dir = controlDir(serve.home);
    const qualified = loadPaths('code_block_lazy_highlight_scroll')[nextWalk++].slice(1).map((s) => s.action);
    const id = await serve.newSession();
    const c: Ctx = {
      page, serve, id, dir, n: 0,
      mounted: false, visible: false, lang: 'known', onSession: false,
      other: { mounted: false, visible: false, lang: 'known', rendered: 'none' },
      qualified, cursor: 0,
    };
    // Both files exist once, up front: mounting a block is only ever
    // reading one, never writing it. tools.write needs a git checkout (a
    // local session's cwd is read-only outside one), so the files are
    // made with a plain shell heredoc instead.
    const heredoc = (path: string, body: string) => `cat > ${path} <<'BOUGH_EOF'\n${body}BOUGH_EOF\n`;
    await send(c, '```js\ntools.bash(' + JSON.stringify(heredoc(PATH.known, BODY.known)) + ');\n'
      + 'tools.bash(' + JSON.stringify(heredoc(PATH.unknown, BODY.unknown)) + ');\n```');
    await page.goto(`${serve.url}/#/`);
    await page.locator('.sidebar').waitFor();
    return c;
  },

  actions: {
    MountKnown: (c) => (instanceOf(c) === 0 ? mount(c, 'known') : Promise.resolve(mountOther(c, 'known'))),
    MountUnknown: (c) => (instanceOf(c) === 0 ? mount(c, 'unknown') : Promise.resolve(mountOther(c, 'unknown'))),
    ScrollIntoView: (c) => (instanceOf(c) === 0 ? scrollIntoView(c) : Promise.resolve(scrollIntoViewOther(c))),
    ScrollAway: (c) => (instanceOf(c) === 0 ? scrollAway(c) : Promise.resolve(scrollAwayOther(c))),
  },

  read: readUiState,
  status: (c) => c.page.locator('.sidebar'),
  sessions: (c) => [c.id],
});
