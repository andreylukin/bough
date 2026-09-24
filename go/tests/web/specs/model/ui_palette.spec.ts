// go/tests/model/specs/ui_palette.fizz in the browser: the palette (⌘K,
// ⌘P, ⌥N, the New button) walked path by path with real keys and clicks
// against one serve per worker, whose model is llm-control. Recipe:
// go/tests/model/README.md.
//
// The spec's server steps are held open by routes, so the page sits in
// the state the path says until the step that ends it:
//   /api/search       held until SearchDone (answered) or SearchFail (500)
//   /api/dirs         held until DirsAnswer
//   POST /api/sessions  held until Created, then forwarded to serve
// S's running turn is a llm-control "block" turn prompted over the API,
// as another tab or the CLI would, and released at TurnEnds.
//
// Every node also owes what any screen does (helpers/ui-invariants.ts):
// no clipped text in the palette's headers and status lines, a visible
// ring on the focused element, and axe clean on whatever is up.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { APIResponse, Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';
import { uiInvariants } from '../../helpers/ui-invariants';

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;       // llm-control's dir
  s: string;         // S, the one session the fixture has
  phrase: string;    // said in S's transcript only: the text query
  turn: string;      // S's held turn, '' when none
  search: Route[];   // held /api/search reads
  dirs: Route[];     // held /api/dirs reads
  post: Route | null; // the held create
  ids: string[];     // sessions this walk made, for the trace check
  carrier: string;   // what the last read found up: pal | dialog | creating | row
}

// The serve's HOME is made under os.tmpdir(), which on macOS is a
// symlink (/var -> /private/var). serve names home as HOME says and a
// session's cwd as the child resolved it, so S "in home" read as another
// folder and the palette offered "New session in <tmp dir>" beside home.
// The spec's fixture is a home with one name.
process.env.TMPDIR = fs.realpathSync(os.tmpdir());

// A worker runs its tests one after another on one serve: names only
// need to be unique within it, and to sort in the order they are queued.
let serial = 0;
const stamp = () => `${Date.now().toString(36)}-${String(++serial).padStart(4, '0')}`;

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 10_000): Promise<void> {
  for (const end = Date.now() + ms; !(await ok()); await new Promise((r) => setTimeout(r, 20))) {
    if (Date.now() > end) throw new Error(`ui_palette: ${what} after ${ms}ms`);
  }
}

async function ok(res: APIResponse, what: string): Promise<APIResponse> {
  if (!res.ok()) throw new Error(`ui_palette: ${what}: ${res.status()} ${await res.text()}`);
  return res;
}

// ---- the page ----

const pal = (c: Ctx) => c.page.locator('.pal');
const field = (c: Ctx) => c.page.locator('.pal-field');
const folderDialog = (c: Ctx) => c.page.getByRole('dialog', { name: 'Start in folder' });
const creating = (c: Ctx) => c.page.getByRole('status').filter({ hasText: 'Starting a session…' });
const sRow = (c: Ctx) => c.page.locator(`button.row[data-id="${c.s}"]`).first();

// The keyboard reaches the page wherever focus is; the palette's field
// does not stop ⌘K, ⌘P or ⌥N.
const key = (c: Ctx, k: string) => c.page.keyboard.press(k);

// ---- readUiState ----

const MODES: Record<string, string> = {
  'Switch to a session…': 'switch',
  'Start in a folder, or type a first message…': 'new',
  'Search sessions or run a command…': 'all',
};

function queryKind(q: string): string {
  if (q === '') return 'empty';
  if (q === '~/wor') return 'path';
  if (q === '~/work/') return 'full';
  if (q === '~/nope') return 'nopath';
  return /^(\/|~(\/|$))/.test(q) ? `unknown path: ${q}` : 'text';
}

// A row's kind, as fizz names it, from its option id and hint.
function rowKind(c: Ctx, id: string, hint: string): string {
  const k = id.replace(/^pal-/, '');
  if (k === `s:${c.s}`) return 'session';
  if (k === 'new:here') return 'cmd';
  if (k === 'new:folder') return 'ask';
  if (k === 'new:project') return 'project';
  if (k === 's:changes' || k === 's:context') return 'this';
  if (k === 's:stop') return 'stop';
  if (k.startsWith('dir:')) return hint.includes('Folder not found') ? 'missing' : 'dir';
  if (k.startsWith('start:')) return 'start';
  return `unknown row: ${k}`;
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const hash = await p.evaluate(() => window.location.hash);
  const route = hash === `#/s/${c.s}` ? 's' : /^#\/s\/./.test(hash) ? 'new' : ['', '#', '#/'].includes(hash) ? 'list' : `unknown: ${hash}`;
  const label = (await sRow(c).getAttribute('aria-label')) ?? '';
  const running = label.split(', ')[1] === 'Running';

  const open = await pal(c).isVisible();
  let mode = 'all', aimed = false, query = 'empty', search = 'idle', dirs = 'none', at = '';
  if (open) {
    const ph = (await field(c).getAttribute('placeholder')) ?? '';
    mode = MODES[ph] ?? `unknown: ${ph}`;
    // Where Start lands, said next to the field (see palette.tsx).
    aimed = await pal(c).locator('.pal-aim').isVisible();
    query = queryKind((await field(c).inputValue()).trim());
    if (await pal(c).locator('.pal-count', { hasText: 'Searching…' }).isVisible()) search = 'loading';
    else if (await pal(c).getByText('Text search failed').isVisible()) search = 'error';
    else if (await pal(c).locator('.pal-group', { hasText: 'Mentioned in' }).isVisible()) search = 'done';
    if (['path', 'full', 'nopath'].includes(query)) dirs = (await pal(c).locator('[id^="pal-dir:"]').count()) > 0 ? 'answered' : 'pending';
    // The first row is Enter's by default; any other was picked with the arrows.
    const opts = pal(c).getByRole('option');
    const n = await opts.count();
    for (let i = 0; i < n; i++) {
      const o = opts.nth(i);
      if ((await o.getAttribute('aria-selected')) !== 'true') continue;
      if (i > 0) at = await o.evaluate((el) => [el.id, el.querySelector('.pal-hint')?.textContent ?? ''] as const).then(([id, hint]) => rowKind(c, id, hint));
      break;
    }
  }
  const dialog = await folderDialog(c).isVisible();
  const busy = await creating(c).isVisible();
  c.carrier = open ? 'pal' : dialog ? 'dialog' : busy ? 'creating' : 'row';
  const edited = dialog && (await folderDialog(c).getByRole('button', { name: 'Start' }).isEnabled());
  const focus = await p.evaluate(() => {
    const el = document.activeElement;
    if (el?.classList.contains('pal-field')) return 'field';
    if (el?.matches('.dlg[aria-labelledby="dlg-title"] input')) return 'dialog';
    const modal = el?.closest('[aria-modal="true"]');
    return modal ? `in another modal: ${el?.tagName}.${el?.className}` : 'page';
  });
  return {
    route, running, open, mode, aimed, query, search, dirs, at, dialog, edited, focus,
    creating: busy,
  };
}

// ---- the server's side, held by routes ----

async function held(what: string, list: () => Route[]): Promise<Route[]> {
  await until(`no ${what} request from the page`, () => list().length > 0);
  return list().splice(0);
}

// Reads the page gave up on (a newer query aborted them) are let go too.
const go = (r: Route) => r.continue().catch(() => {});

async function answerSearch(c: Ctx, fail: boolean): Promise<void> {
  const rs = await held('/api/search', () => c.search);
  const last = rs.pop()!;
  for (const r of rs) await go(r);
  if (fail) await last.fulfill({ status: 500, contentType: 'application/json', body: '{"error":"ui_palette: search failed on purpose"}' }).catch(() => {});
  else await go(last);
}

// ---- Init: one session S in home, said the phrase, nothing else listed ----

async function reset(c: Ctx): Promise<void> {
  const { api, home } = c.serve;
  // Earlier walks on this worker's serve: every session they made goes
  // to the archive (their children with them), and no turn stays queued.
  const all = (await (await ok(await api.get('/api/sessions?all=1'), 'list')).json()).sessions as { id: string; archived: boolean }[];
  for (const r of all.filter((x) => !x.archived)) {
    await ok(await api.post(`/api/sessions/${r.id}/archive`, { data: { stopChildren: true } }), `archive ${r.id}`);
  }
  fs.mkdirSync(c.dir, { recursive: true });
  for (const f of fs.readdirSync(c.dir)) if (f.endsWith('.json')) fs.rmSync(path.join(c.dir, f), { force: true });

  const name = stamp();
  c.phrase = `zyqv${name.replace(/[^a-z0-9]/g, '')}`;
  queue(c.dir, name, { mode: 'ok', text: `noted: ${c.phrase}` });
  const res = await ok(await api.post('/api/sessions', { data: { cwd: home, prompt: 'hello' } }), 'create S');
  c.s = (await res.json()).session.id;
  c.ids.push(c.s);
  await waitTaken(c.dir, name);
  await until('S never finished its first turn', async () => {
    const r = await api.get(`/api/sessions/${c.s}`);
    const row = r.ok() ? (await r.json()).session : null;
    return row && row.status !== 'running' && !row.empty;
  }, 20_000);
}

modelTests<Ctx>({
  spec: 'ui_palette',
  role: 'Palette#0',
  config: CONTROL_CONFIG,
  shared: true,
  // Reads move the page's clock 1 s: enough for the list poll to come
  // round within a few reads, and far from the 15 s a held search or
  // folder read takes to give up by itself.
  pollStepMs: 1_000,

  async init(page, serve) {
    test.info().setTimeout(60_000);
    const c: Ctx = { page, serve, dir: controlDir(serve.home), s: '', phrase: '', turn: '', search: [], dirs: [], post: null, ids: [], carrier: 'row' };
    await reset(c);
    await page.addInitScript(() => { try { localStorage.setItem('bough:welcome-done', '1'); } catch { /* storage off */ } });
    await page.route((u) => u.pathname === '/api/search', (r) => { c.search.push(r); });
    await page.route((u) => u.pathname === '/api/dirs', (r) => { c.dirs.push(r); });
    await page.route((u) => u.pathname === '/api/sessions', (r) => {
      if (r.request().method() !== 'POST') return r.continue();
      c.post = r;
    });
    // Landing on #/ on a desktop opens the newest session by itself
    // (arrivalPick), once; the spec starts on the list with none open, as
    // after going back to it. So the page lands elsewhere and then goes
    // to the list, and that arrival is spent.
    await page.goto(`${serve.url}/#/hooks`);
    await sRow(c).waitFor();
    await page.evaluate(() => { window.location.hash = '#/'; });
    return c;
  },

  actions: {
    OpenAll: (c) => key(c, 'ControlOrMeta+k'),
    OpenSwitch: (c) => key(c, 'ControlOrMeta+p'),
    OpenNew: (c) => key(c, 'Alt+n'),
    NewButton: (c) => c.page.locator('button.side-new').click(),

    TypeText: (c) => field(c).fill(c.phrase),
    TypePath: (c) => field(c).fill('~/wor'),
    TypeMissing: (c) => field(c).fill('~/nope'),
    async Clear(c) {
      await field(c).press('ControlOrMeta+a');
      await field(c).press('Backspace');
    },
    Complete: (c) => field(c).press('Tab'),

    SearchDone: (c) => answerSearch(c, false),
    SearchFail: (c) => answerSearch(c, true),
    Retry: (c) => pal(c).getByRole('button', { name: 'Retry' }).click(),
    async DirsAnswer(c) {
      for (const r of await held('/api/dirs', () => c.dirs)) await go(r);
    },

    // Another client prompts S; its turn holds until TurnEnds.
    async TurnStarts(c) {
      c.turn = stamp();
      queue(c.dir, c.turn, { mode: 'block', text: 'done' });
      await ok(await c.serve.api.post(`/api/sessions/${c.s}/prompt`, { data: { text: 'keep going' } }), 'prompt S');
      await waitTaken(c.dir, c.turn);
    },
    async TurnEnds(c) {
      release(c.dir, c.turn);
      c.turn = '';
    },

    Down: (c) => field(c).press('ArrowDown'),
    Up: (c) => field(c).press('ArrowUp'),

    PickSession: (c) => field(c).press('Enter'),
    PickStart: (c) => field(c).press('Enter'),
    PickDir: (c) => field(c).press('Enter'),
    PickMissing: (c) => field(c).press('Enter'),
    PickAsk: (c) => field(c).press('Enter'),

    DialogType: (c) => folderDialog(c).getByRole('textbox').fill('~/work'),
    DialogStart: (c) => folderDialog(c).getByRole('button', { name: 'Start' }).click(),
    DialogCancel: (c) => folderDialog(c).getByRole('textbox').press('Escape'),

    async Created(c) {
      const r = c.post;
      if (!r) throw new Error('ui_palette: no create in flight');
      c.post = null;
      const res = await r.fetch();
      if (res.status() === 201) c.ids.push((await res.json()).session.id);
      await r.fulfill({ response: res });
    },

    Escape: (c) => field(c).press('Escape'),
    Back: async (c) => { await c.page.goBack(); },
    // Across 720 px and back: the crossing closes the palette.
    async Resize(c) {
      const size = c.page.viewportSize()!;
      await c.page.setViewportSize({ width: 700, height: size.height });
      await pal(c).waitFor({ state: 'hidden' });
      await c.page.setViewportSize(size);
    },
  },

  // SearchFail's 500: Chromium logs the status; the page's answer is the
  // "Text search failed" line.
  allowConsole: /^Failed to load resource: the server responded with a status of 500 /,

  read: readUiState,
  // What carries the state: the modal that is up, the create's status
  // line, or else S's row, which says whether its turn runs.
  status: (c) => ({ pal, dialog: folderDialog, creating, row: sRow } as Record<string, (c: Ctx) => ReturnType<Page['locator']>>)[c.carrier](c),
  async invariants(c, where) {
    // The palette fades in: axe measuring contrast mid-fade reads the
    // text as fainter than it is. Finite animations only (spinners loop).
    await c.page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => Number.isFinite(Number(a.effect?.getComputedTiming().endTime)))
      .map((a) => a.finished.catch(() => undefined))));
    await uiInvariants(c.page, {
      include: ['.pal', '.dlg', '.updated'],
      text: '.pal-group, .pal-label, .pal-hint, .pal-count, .pal-k, .pal-none-1, .pal-none-2, .pal-tip, .pal-mode, .pal-aim, .dlg-title, .updated-text',
    }, where);
  },
  sessions: (c) => c.ids,
  async cleanup(c) {
    for (const r of [...c.search.splice(0), ...c.dirs.splice(0)]) await go(r);
    await c.post?.abort().catch(() => {});
    c.post = null;
    if (c.turn) release(c.dir, c.turn);
  },
});
