// go/tests/model/specs/session_create.fizz in the browser: a local
// session started from the web page (the palette's New, its folder rows
// and "a folder…" prompt, the Overview's New session button), walked
// path by path against a real serve whose model is llm-control. Recipe:
// go/tests/model/README.md; the Go twin is mbt/session_create_test.go.
//
// The spec splits a create into steps the product runs back to back, so
// the walk holds each one open the way the Go adapter does:
//   - llm-control's start hold parks the spawned child before its
//     history file (WriteHistory lifts it, ChildExits makes it exit);
//   - the page's POST /api/sessions is fetched by a route and its answer
//     handed to the page only at Claim, Timeout or ChildExits;
//   - after a typed start, the new session's reads are held until Land,
//     which is the window where the page shows its "Sending…" preview.
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import type { APIResponse, Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

interface Answer { route: Route; res: Promise<APIResponse> }

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;       // llm-control's dir: the start hold lives there
  hist: string;      // the serve's history dir
  n: number;
  prompt: string;    // the typed first message of the create, '' when empty
  first: string;     // the create in flight: none | text | empty
  other: string;     // the unrelated history file OtherWrites wrote
  before: Set<string> | null; // history ids before the create's POST
  pid: number;       // the child parked at the hold
  id: string;        // the opened session
  ids: string[];     // every session opened, for the trace check
  post: Answer | null;           // the POST's answer, not yet given to the page
  held: (() => void)[];          // the opened session's reads, until Land
  hold: ((u: URL) => boolean) | null; // the route that holds them
  carrier: string;   // the element the last read took ui from
}

// ---- llm-control's start hold (the Go side is tests/model/llm) ----

function holdStart(c: Ctx): void {
  fs.mkdirSync(c.dir, { recursive: true });
  fs.rmSync(path.join(c.dir, 'start.exit'), { force: true });
  // A child killed while held never removed its marker.
  for (const f of fs.readdirSync(c.dir)) if (/^start-\d+\.held$/.test(f)) fs.rmSync(path.join(c.dir, f), { force: true });
  fs.writeFileSync(path.join(c.dir, 'start.hold'), '');
}
const releaseStart = (c: Ctx) => fs.rmSync(path.join(c.dir, 'start.hold'), { force: true });
const exitStart = (c: Ctx) => fs.writeFileSync(path.join(c.dir, 'start.exit'), '');
function heldPid(c: Ctx): number {
  for (const f of fs.existsSync(c.dir) ? fs.readdirSync(c.dir) : []) {
    const m = /^start-(\d+)\.held$/.exec(f);
    if (m) return Number(m[1]);
  }
  return 0;
}
function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch { return false; }
}
const histIds = (c: Ctx) => (fs.existsSync(c.hist) ? fs.readdirSync(c.hist) : [])
  .filter((f) => f.endsWith('.jsonl')).map((f) => f.slice(0, -'.jsonl'.length));

async function until(what: string, ok: () => boolean, ms = 15_000): Promise<void> {
  for (const end = Date.now() + ms; !ok(); await new Promise((r) => setTimeout(r, 20))) {
    if (Date.now() > end) throw new Error(`session_create: ${what} after ${ms}ms`);
  }
}

// ---- the page ----

const palette = (c: Ctx) => c.page.locator('.pal');
const field = (c: Ctx) => c.page.locator('.pal-field');
const folderDialog = (c: Ctx) => c.page.getByRole('dialog', { name: 'Start in folder' });
const toast = (c: Ctx) => c.page.locator('.toast').filter({ hasText: 'Couldn’t start a session' });
const overviewNew = (c: Ctx) => c.page.locator('button.ov-new');
const folderRow = (c: Ctx) => c.page.locator('[id^="pal-dir:"]', { hasText: 'New session in work' });
const overview = (c: Ctx) => c.page.getByRole('heading', { name: 'Overview', level: 1 });
// What the page shows while its POST /api/sessions is in flight.
const creating = (c: Ctx) => c.page.getByRole('status').filter({ hasText: 'Starting a session…' });

// A start: the child will park at the hold, and the page's POST is kept
// from it until the spec says how the create ends.
async function start(c: Ctx, first: 'text' | 'empty', click: () => Promise<void>): Promise<void> {
  holdStart(c);
  c.before = new Set(histIds(c));
  c.first = first;
  c.pid = 0;
  await click();
  await until('no POST /api/sessions from the page', () => c.post !== null);
  await until('no child parked at the hold', () => (c.pid = heldPid(c)) !== 0);
}

// The page learns the POST's answer. A 201 after a typed start holds
// the new session's reads first, so the preview stays until Land.
async function answer(c: Ctx, typed: boolean): Promise<void> {
  const p = c.post;
  if (!p) throw new Error('session_create: no create in flight');
  c.post = null;
  const res = await p.res;
  releaseStart(c);
  if (res.status() === 201) {
    const id = (await res.json()).session.id as string;
    c.id = id;
    c.ids.push(id);
    if (typed) {
      c.hold = (u) => u.pathname.startsWith(`/api/sessions/${id}`);
      await c.page.route(c.hold, (r) => {
        c.held.push(() => { void r.continue().catch(() => {}); });
      });
    }
  } else {
    // A failed create's child must be gone; give it a moment to be reaped.
    await until(`child ${c.pid} still alive after a failed create`, () => !alive(c.pid), 5_000).catch(() => {});
  }
  await p.route.fulfill({ response: res });
}

// Forget the create, as the spec's Close and Dismiss do.
function forget(c: Ctx): void {
  c.first = 'none';
  c.prompt = '';
  c.other = '';
  c.before = null;
  c.pid = 0;
  c.id = '';
}

// ---- readUiState ----
//
// ui, query, sending and recorded are the page's own state and come from
// the DOM only; so does `first` once a session is open (its thread shows
// the typed prompt or nothing). `first` before that, and child, hist and
// other, are not the page's: the page cannot show which button a failed
// create came from, a parked process or a history file. Those are read
// the way the Go adapter reads them, off the serve's process and files,
// and are there so a path's state is compared whole.
async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const hash = await p.evaluate(() => window.location.hash);
  let ui: string;
  if (await palette(c).isVisible()) { ui = 'palette'; c.carrier = '.pal'; }
  else if (await folderDialog(c).isVisible()) { ui = 'folder'; c.carrier = 'folder'; }
  else if (await toast(c).isVisible()) { ui = 'failed'; c.carrier = 'toast'; }
  else if (c.id && hash === `#/s/${c.id}`) {
    const starting = await p.getByText('Starting the session…').isVisible();
    ui = starting ? 'starting' : 'opened';
    c.carrier = 'thread';
  } else if (await creating(c).isVisible()) { ui = 'creating'; c.carrier = 'creating'; }
  else if (await overview(c).isVisible()) { ui = 'idle'; c.carrier = 'overview'; }
  else { ui = `unknown: ${hash}`; c.carrier = 'main'; }

  const q = ui === 'palette' ? (await field(c).inputValue()).trim() : '';
  const query = !q ? 'none' : /^(\/|~(\/|$))/.test(q) ? 'path' : 'text';

  const thread = p.locator('main .thread');
  const sending = ui === 'opened' && (await thread.locator('.turn-sending-state', { hasText: 'Sending…' }).count()) > 0;
  const recorded = ui === 'opened' && (await thread.locator('section.turn:not(.turn-sending) .prompt-text').count()) > 0;
  const first = ui === 'opened' ? (sending || recorded ? 'text' : 'empty') : c.first;

  let child = 'none';
  if (ui === 'opened' && (await p.locator(`button.row[data-id="${c.id}"]`).count()) > 0) child = 'live';
  else if (c.pid && alive(c.pid)) child = 'spawned';

  let hist = false;
  if (c.before) {
    hist = c.id ? fs.existsSync(path.join(c.hist, c.id + '.jsonl'))
      : histIds(c).some((id) => !c.before!.has(id) && id !== c.other);
  }
  return { ui, query, first, child, hist, other: c.other !== '', sending, recorded };
}

const CARRIERS: Record<string, (c: Ctx) => ReturnType<Page['locator']>> = {
  '.pal': palette,
  folder: folderDialog,
  toast,
  thread: (c) => c.page.locator('main .thread').first(),
  creating,
  overview,
  main: (c) => c.page.locator('main'),
};

modelTests<Ctx>({
  spec: 'session_create',
  role: 'Create#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    // A Timeout step is the supervisor's real 10 s create timeout.
    test.info().setTimeout(90_000);
    const c: Ctx = {
      page, serve, dir: controlDir(serve.home), hist: path.join(serve.home, '.bough', 'history'),
      n: 0, prompt: '', first: 'none', other: '', before: null, pid: 0, id: '', ids: [],
      post: null, held: [], hold: null, carrier: 'main',
    };
    // The welcome stands in for the Overview on an empty server; the
    // spec starts at the Overview.
    await page.addInitScript(() => { try { localStorage.setItem('bough:welcome-done', '1'); } catch { /* storage off */ } });
    await page.route((u) => u.pathname === '/api/sessions', async (r) => {
      if (r.request().method() !== 'POST') return r.continue();
      c.post = { route: r, res: r.fetch({ timeout: 60_000 }) };
    });
    await page.goto(`${serve.url}/#/`);
    return c;
  },

  actions: {
    // The sidebar's New button: the palette, aimed at no folder.
    OpenPalette: (c) => c.page.locator('button.side-new').click(),
    async Escape(c) {
      if (await folderDialog(c).isVisible()) await folderDialog(c).getByRole('textbox').press('Escape');
      else await field(c).press('Escape');
    },
    TypeText: (c) => field(c).fill(`first prompt ${String(++c.n).padStart(4, '0')}`),
    async TypePath(c) {
      await field(c).fill('~/work');
      // The folder rows come from GET /api/dirs.
      await folderRow(c).waitFor();
    },
    // A folder row aims the palette there and clears the query.
    PickFolder: (c) => folderRow(c).click(),
    async StartTyped(c) {
      c.prompt = await field(c).inputValue();
      await start(c, 'text', () => c.page.locator('[id^="pal-start:"]').click());
    },
    // "New session in <folder>": searched for when something else is typed.
    async StartEmpty(c) {
      if (await field(c).inputValue()) await field(c).fill('New session in home');
      await start(c, 'empty', () => c.page.locator('[id="pal-new:here"]').click());
    },
    // The Overview's New session button is display:none above 720 px,
    // and at 720 px or less the Overview is never on screen (the list
    // pane is), so no one can click it. Its handler is still the page's
    // own start(home, ""), which is what the spec is about: it is fired
    // on the element directly.
    OverviewNew: (c) => start(c, 'empty', () => overviewNew(c).dispatchEvent('click')),
    async AskFolder(c) {
      if (await field(c).inputValue()) await field(c).fill('a folder');
      await c.page.locator('[id="pal-new:folder"]').click();
    },
    async FolderStart(c) {
      await folderDialog(c).getByRole('textbox').fill('~/work');
      await start(c, 'empty', () => folderDialog(c).getByRole('button', { name: 'Start' }).click());
    },
    // Not a directory: serve refuses the POST (400) and spawns nothing.
    async FolderMissing(c) {
      await folderDialog(c).getByRole('textbox').fill('~/no-such-folder');
      await folderDialog(c).getByRole('button', { name: 'Start' }).click();
      await until('no POST /api/sessions from the page', () => c.post !== null);
      await answer(c, false);
    },
    async WriteHistory(c) {
      releaseStart(c);
      await until('the released child wrote no history', () => histIds(c).some((id) => !c.before!.has(id) && id !== c.other));
    },
    // An unrelated bough starts a session while the create waits: a
    // history file of its own, written as that process would.
    async OtherWrites(c) {
      const hex = Date.now().toString(16).padStart(12, '0');
      const rnd = [...crypto.getRandomValues(new Uint8Array(10))].map((b) => b.toString(16).padStart(2, '0')).join('');
      c.other = `${hex.slice(0, 8)}-${hex.slice(8)}-7${rnd.slice(0, 3)}-8${rnd.slice(3, 6)}-${rnd.slice(6, 18)}`;
      fs.mkdirSync(c.hist, { recursive: true });
      const meta = { seq: 1, at: new Date().toISOString(), kind: 'meta', data: { cwd: c.serve.home, mode: 'local', origin: 'tui' } };
      fs.writeFileSync(path.join(c.hist, c.other + '.jsonl'), JSON.stringify(meta) + '\n');
    },
    Claim: (c) => answer(c, c.first === 'text'),
    Timeout: (c) => answer(c, false),
    async ChildExits(c) {
      exitStart(c);
      await answer(c, false);
    },
    // The new session's reads go through: the recorded prompt replaces the preview.
    async Land(c) {
      if (c.hold) await c.page.unroute(c.hold);
      c.hold = null;
      for (const go of c.held.splice(0)) go();
    },
    // Back to the Overview; the session lives on under its row.
    async Close(c) {
      await c.page.goBack();
      forget(c);
    },
    async Dismiss(c) {
      await toast(c).getByRole('button', { name: 'Dismiss' }).click();
      forget(c);
    },
  },

  // FolderMissing's 400, Timeout's and ChildExits' 500: Chromium logs
  // the POST's status; the page's own answer to it is the toast.
  allowConsole: /^Failed to load resource: the server responded with a status of (400|500) /,

  read: readUiState,
  status: (c) => CARRIERS[c.carrier](c),
  sessions: (c) => c.ids,
  async cleanup(c) {
    for (const go of c.held.splice(0)) go();
    if (c.post) {
      exitStart(c);
      await c.post.res.catch(() => undefined);
      await c.post.route.abort().catch(() => {});
      c.post = null;
    }
    releaseStart(c);
  },
});
