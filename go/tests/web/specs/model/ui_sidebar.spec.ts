// go/tests/model/specs/ui_sidebar.fizz in the browser: the control
// room's session list walked path by path with real clicks, keys, hovers
// and drags against a real serve whose model is llm-control. Recipe:
// go/tests/model/README.md.
//
// The world the spec names:
//   A  a local session in serve.work, made by Appear (another client's
//      POST) with a llm-control "block" turn, ended by Finish or Fail.
//   p  a project made at Init; B, its thread, is seeded and filed into it
//      by ProjectThread, so p gets a group to drop A on.
//   Z  a seeded session archived at Init: what the Archived section loads.
// Titles: A "alpha task", B "bravo", Z "zulu"; the "hit" query is
// "alpha", the "miss" one "zzqx".
//
// The spec's server steps are held open at the network, so the page sits
// in the state the path says until the step that ends it:
//   GET /api/sessions        held (loading), failed (unavailable, delayed)
//                            or let through, per the walk's listMode
//   GET /api/sessions?all=1  the same for the Archived read (archMode)
//   POST …/A/ack             held from Finish until AutoAck: the page acks
//                            a finish it shows at once, and the spec has
//                            the ack as its own step
//   POST …/A/project         held from Drop until MoveOk or MoveFail
//
// What is read where: the page's own state (the route, the pane, the
// fold, the filter, focus, the card, the rows, pins, dots, Seen, what the
// list says) from the DOM; groupFolded from the page's saved fold
// (bough:ws-folded), because a failed A leaves its folder group and the
// fold is then drawn nowhere; the world's fields (a, unseen, trouble,
// project, proj) from serve, as ui_projects reads its disk, and checked
// against A's row wherever the row is drawn. Two are the walk's record,
// said where they are kept: the filter while the list is folded to the
// rail (nothing on screen holds it), and whether Archived was included
// from a filter (the page keeps that in a ref).
//
// Every node also owes what any screen does (helpers/ui-invariants.ts):
// no clipped text in the list's headers and status lines, a visible ring
// on the focused element, and axe clean on the sidebar and its card.
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import AxeBuilder from '@axe-core/playwright';
import type { APIResponse, Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';
import { uiInvariants } from '../../helpers/ui-invariants';

// serve names a session's cwd as the child resolved it; a HOME under a
// symlinked tmpdir (/var -> /private/var) would put A in a folder whose
// name the walk does not know.
process.env.TMPDIR = fs.realpathSync(os.tmpdir());

// Up to 35 steps a walk, each with an axe pass.
test.describe.configure({ timeout: 300_000 });

const A_TITLE = 'alpha task';
const HIT = 'alpha';
const MISS = 'zzqx';
const DESKTOP = { width: 1100, height: 700 };
const PHONE = { width: 700, height: 700 };

type Mode = 'hold' | 'fail' | 'pass';

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;        // llm-control's dir
  a: string;          // A's id, '' before Appear
  b: string;          // B's id, '' before ProjectThread
  slug: string;       // p's slug
  turn: string;       // A's held turn, '' when none
  listMode: Mode;     // GET /api/sessions
  archMode: Mode;     // GET /api/sessions?all=1
  listHeld: Route[];
  archHeld: Route[];
  holdAck: boolean;   // from Finish until AutoAck
  acks: Route[];
  assign: Route | null;
  search: string;     // the filter as last seen on screen (the rail keeps it off screen)
  included: boolean;  // Archived was opened from its "Include" (under a query)
  carrier: string;    // what carries the state: sidebar | main
}

async function ok(res: APIResponse, what: string): Promise<APIResponse> {
  if (!res.ok()) throw new Error(`ui_sidebar: ${what}: ${res.status()} ${await res.text()}`);
  return res;
}

async function until(what: string, done: () => boolean | Promise<boolean>, ms = 10_000): Promise<void> {
  for (const end = Date.now() + ms; !(await done()); await new Promise((r) => setTimeout(r, 20))) {
    if (Date.now() > end) throw new Error(`ui_sidebar: ${what} after ${ms}ms`);
  }
}

// A history file as serve lists it (the rename keeps serve from reading
// half of one), with one finished turn so it is not an empty session.
function seed(serve: Serve, cwd: string): string {
  const id = crypto.randomUUID();
  const at = new Date().toISOString();
  const lines = [
    { kind: 'meta', data: { cwd, mode: 'local' } },
    { kind: 'input', data: { text: 'earlier work' } },
    { kind: 'done', data: {} },
  ].map((e, i) => JSON.stringify({ seq: i + 1, at, ...e }));
  const dir = path.join(serve.home, '.bough', 'history');
  fs.mkdirSync(dir, { recursive: true });
  fs.mkdirSync(cwd, { recursive: true });
  fs.writeFileSync(path.join(dir, id + '.seed'), lines.join('\n') + '\n');
  fs.renameSync(path.join(dir, id + '.seed'), path.join(dir, id + '.jsonl'));
  return id;
}

// ---- the page ----

const sidebar = (c: Ctx) => c.page.locator('.sidebar');
const field = (c: Ctx) => c.page.locator('#q');
// The recent groups are the tree's own children; Archived's sit in its section.
const groups = '.sidebar .scroll > .ws';
const pinnedRow = (c: Ctx) => c.page.locator(`.sidebar .needs button.row[data-id="${c.a}"]`);
const groupRow = (c: Ctx) => c.page.locator(`${groups} button.row[data-id="${c.a}"]`);
const folderHead = (c: Ctx) => c.page.locator(`${groups} > .ws-head:not([data-project])`);
const projectGroup = (c: Ctx) => c.page.locator(`${groups}:has(> .ws-head[data-project])`);
const archHead = (c: Ctx) => c.page.locator('.sidebar button.sec-fold[aria-controls="sec-archived"]');

/** A's row where it is drawn: its pin first, else its group's copy. */
async function aRow(c: Ctx) {
  if (c.a && await pinnedRow(c).isVisible()) return pinnedRow(c);
  return groupRow(c);
}

// The pointer rests somewhere no row is: after a click, so a row it
// happens to stop on does not open a hover card the path never asked for.
async function park(c: Ctx): Promise<void> {
  const v = c.page.viewportSize()!;
  await c.page.mouse.move(v.width - 4, v.height - 4);
}

async function click(c: Ctx, loc: ReturnType<Page['locator']>): Promise<void> {
  // Bounded, so a control that never takes the click fails its step.
  await loc.click({ timeout: 10_000 });
  await park(c);
}

// ---- readUiState ----

async function server(c: Ctx): Promise<{ a: string; unseen: boolean; trouble: boolean; project: string; proj: boolean }> {
  if (!c.a) return { a: 'none', unseen: false, trouble: false, project: '', proj: !!c.b };
  const r = (await (await ok(await c.serve.api.get(`/api/sessions/${c.a}`), 'read A')).json()).session;
  const a = r.status === 'running' ? 'running' : r.status === 'error' ? 'failed' : r.status === 'done' ? 'done' : `unknown: ${r.status}`;
  return { a, unseen: !!r.unseen, trouble: !!r.trouble, project: r.project === c.slug ? 'p' : r.project ? `other: ${r.project}` : '', proj: !!c.b };
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const world = await server(c);
  const dom = await p.evaluate(({ a, work }) => {
    const vis = (el: Element | null) => !!el && (el as HTMLElement).getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
    const phone = window.matchMedia('(max-width:720px)').matches;
    const hash = window.location.hash;
    const bar = document.querySelector('.sidebar');
    const side = document.querySelector('.sidebar.sidebar-closed') ? 'rail' : vis(bar) ? 'full' : 'hidden';
    const q = document.querySelector<HTMLInputElement>('#q');
    const search = !vis(q) ? 'off' : q!.value === '' ? 'open' : q!.value === 'alpha' ? 'hit' : q!.value === 'zzqx' ? 'miss' : `unknown: ${q!.value}`;
    const fresh = document.querySelector('.sidebar .side-fresh')?.textContent ?? '';
    const rows = fresh.startsWith('Sessions unavailable') ? 'unavailable' : fresh.startsWith('Loading sessions') ? 'loading'
      : fresh.startsWith('Updates delayed') ? 'delayed' : fresh ? `unknown: ${fresh}` : 'ready';
    const pin = a ? document.querySelector(`.sidebar .needs button.row[data-id="${a}"]`) : null;
    const inGroup = a ? document.querySelector(`.sidebar .scroll > .ws button.row[data-id="${a}"]`) : null;
    const head = document.querySelector('.sidebar .scroll > .ws > .ws-head:not([data-project])');
    const aRow = vis(pin) ? 'pinned' : vis(inGroup) ? 'group' : a && head?.getAttribute('aria-expanded') === 'false' ? 'folded' : 'none';
    const drawn = [pin, inGroup].filter((el) => vis(el)) as HTMLElement[];
    const dot = drawn.some((el) => !!el.querySelector('.unseen-dot'));
    const seenBtn = drawn.some((el) => vis(el.closest('.row-wrap')?.querySelector('.row-ack') ?? null));
    const card = vis(document.querySelector('#row-card')) && drawn.some((el) => el.getAttribute('aria-describedby') === 'row-card');
    // Archived: its header's fold and count, and what its body says.
    const sec = [...document.querySelectorAll('.sidebar .sec')].find((s) => s.querySelector('[aria-controls="sec-archived"]'));
    const fold = sec?.querySelector('button.sec-fold');
    const body = sec?.querySelector('#sec-archived');
    const counted = !!fold?.querySelector('.sec-count');
    let arch: string;
    if (!fold) arch = 'off';
    else if (body?.querySelector('.pending')) arch = 'loading';
    else if (/Couldn’t load archived/.test(body?.textContent ?? '')) arch = 'failed';
    else if (counted) arch = fold.getAttribute('aria-expanded') === 'true' ? 'open' : 'folded';
    else arch = fold.getAttribute('aria-expanded') === 'false' ? 'off' : 'unknown: open without a count';
    const none = document.querySelector('.sidebar .scroll > .list-none')?.textContent ?? '';
    const tree = document.querySelector('.sidebar .scroll');
    const anything = !!tree?.querySelector('button.row, .ws-head') || (counted && fold!.querySelector('.sec-count')!.textContent!.replace(/\D/g, '') !== '0');
    let says: string;
    if (side !== 'full') says = side;
    else if (rows !== 'ready') says = rows;
    else if (anything) says = 'rows';
    else if (arch === 'loading' || arch === 'failed') says = 'archived';
    else if (none.startsWith('No sessions match')) says = 'nomatch';
    else if (none === 'No sessions yet.') says = 'empty';
    else says = `unknown: ${none}`;
    let folded = false;
    try { folded = (JSON.parse(localStorage.getItem('bough:ws-folded') ?? '[]') as string[]).includes(`recent:${work}`); } catch { /* storage off */ }
    return {
      viewport: phone ? 'phone' : 'desktop',
      pane: document.querySelector('.app')?.getAttribute('data-pane') ?? 'unknown',
      route: ['', '#', '#/'].includes(hash) ? 'home' : a && hash === `#/s/${a}` ? 'session' : hash === '#/hooks' ? 'page' : `unknown: ${hash}`,
      closed: document.querySelector('.side-collapse')?.getAttribute('aria-expanded') === 'false',
      search, inField: document.activeElement?.id === 'q', rows, groupFolded: folded,
      arch: side === 'full' ? arch : 'off', card, side, says, aRow, dot, seenBtn,
      drag: (window as unknown as { __sbDrag?: boolean }).__sbDrag === true,
      over: !!document.querySelector('.sidebar .ws[data-drop="over"]'),
    };
  }, { a: c.a, work: c.serve.work });

  // The filter while the list is off screen is the walk's record of what
  // it last showed; RailFilter and ToggleSide read it back on screen.
  if (dom.side === 'full') c.search = dom.search; else dom.search = c.search;
  if (dom.arch === 'off') c.included = false;
  c.carrier = dom.side === 'hidden' ? 'main' : 'sidebar';
  // Pending is the move in flight: the row stays where it was until it lands.
  const move = c.assign ? 'pending' : dom.over ? 'over' : dom.drag ? 'dragging' : 'none';
  const { drag: _d, over: _o, ...rest } = dom;
  return {
    ...rest, ...world, move,
    archFromFilter: c.included && rest.arch !== 'off' && ['hit', 'miss'].includes(dom.search),
  };
}

// ---- axe on the list ----

// The tree (.scroll, role=tree) owns controls that are not tree items: a
// troubled row's Seen button, and the Archived section's loading status
// and its failure's alert and Retry. axe's aria-required-children flags
// each; the list needs a structural change (a treegrid, or the notices
// moved out of the tree) that is not this walk's to make. Only those
// known children are let through, printed so a run shows them; any other
// violation, or this rule on anything else, still fails the node.
const KNOWN_TREE_CHILDREN = /^Fix any of the following:\s+Element has children which are not allowed: ((button\[aria-label\]|button|\[role=status\]|\[role=alert\]|div\[role=status\]|span\[role=alert\]|div\[role=alert\])(, )?)+$/;
const tolerated = new Set<string>();

async function sidebarAxe(c: Ctx, where: string): Promise<void> {
  if (!(await c.page.locator('.sidebar').count())) return;
  // Judged at rest: a fade measured halfway reads as low contrast. CSS
  // runs on real time, not the walk's clock, and a paused animation never
  // finishes, so the wait is bounded here.
  await Promise.race([
    c.page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
      .map((a) => a.finished.catch(() => {})))),
    new Promise((r) => setTimeout(r, 2_000)),
  ]);
  const res = await new AxeBuilder({ page: c.page }).include('.sidebar').analyze();
  const found: string[] = [];
  for (const v of res.violations) {
    for (const n of v.nodes) {
      const why = (n.failureSummary ?? '').replace(/\s+/g, ' ').trim();
      const line = `${v.id}: ${n.target.join(' ')}: ${why}`;
      if (v.id === 'aria-required-children' && /role="tree"|\.scroll/.test(n.target.join(' ')) && KNOWN_TREE_CHILDREN.test(why)) {
        if (!tolerated.has(why)) { tolerated.add(why); console.log(`ui_sidebar: known finding, not failed: ${line}`); }
        continue;
      }
      found.push(line);
    }
  }
  expect(found, `${where}: axe on .sidebar`).toEqual([]);
}

// ---- the server's side ----

const go = (r: Route) => r.continue().catch(() => {});
const refuse = (r: Route, status: number) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify({ error: `ui_sidebar: refused on purpose (${status})` }) }).catch(() => {});

function settle(held: Route[], how: 'pass' | 'fail'): Promise<unknown> {
  return Promise.all(held.splice(0).map((r) => (how === 'pass' ? go(r) : refuse(r, 500))));
}

async function newTurn(c: Ctx): Promise<string> {
  const name = `t${Date.now().toString(36)}`;
  queue(c.dir, name, { mode: 'block', text: 'done here' });
  return name;
}

modelTests<Ctx>({
  spec: 'ui_sidebar',
  role: 'Sidebar#0',
  config: CONTROL_CONFIG,
  // Chromium logs the answers the walk refuses on purpose: the list's
  // 500s (unavailable, delayed, Archived's failure) and the move's 409.
  allowConsole: /^Failed to load resource: the server responded with a status of (409|500) /,

  async init(page, serve) {
    const c: Ctx = {
      page, serve, dir: controlDir(serve.home), a: '', b: '', slug: '', turn: '',
      listMode: 'hold', archMode: 'hold', listHeld: [], archHeld: [], holdAck: false, acks: [], assign: null,
      search: 'off', included: false, carrier: 'sidebar',
    };
    fs.mkdirSync(c.dir, { recursive: true });
    const res = await ok(await serve.api.post('/api/projects', { data: { name: 'Pgroup' } }), 'create p');
    c.slug = (await res.json()).project.slug;
    const z = seed(serve, path.join(serve.home, 'zsrc'));
    await ok(await serve.api.post(`/api/sessions/${z}/rename`, { data: { title: 'zulu' } }), 'rename Z');
    await ok(await serve.api.post(`/api/sessions/${z}/archive`, { data: {} }), 'archive Z');

    await page.addInitScript(() => {
      try { localStorage.setItem('bough:welcome-done', '1'); } catch { /* storage off */ }
      // The browser's drag, as the page's drag handlers see it.
      const w = window as unknown as { __sbDrag?: boolean };
      window.addEventListener('dragstart', () => { w.__sbDrag = true; }, true);
      window.addEventListener('dragend', () => { w.__sbDrag = false; }, true);
    });
    await page.route((u) => u.pathname === '/api/sessions', (r) => {
      if (r.request().method() !== 'GET') return go(r);
      const all = new URL(r.request().url()).searchParams.get('all') === '1';
      const mode = all ? c.archMode : c.listMode;
      if (mode === 'pass') return go(r);
      if (mode === 'fail') return refuse(r, 500);
      (all ? c.archHeld : c.listHeld).push(r);
    });
    await page.route((u) => /^\/api\/sessions\/[^/]+\/ack$/.test(u.pathname), (r) => {
      if (c.holdAck && r.request().url().includes(`/${c.a}/ack`)) c.acks.push(r);
      else void go(r);
    });
    await page.route((u) => /^\/api\/sessions\/[^/]+\/project$/.test(u.pathname), (r) => {
      if (r.request().method() === 'POST') c.assign = r;
      else void go(r);
    });
    await page.setViewportSize(DESKTOP);
    await page.goto(`${serve.url}/#/`);
    await until('the first list read never went out', () => c.listHeld.length > 0);
    return c;
  },

  actions: {
    // ---- the list read ----
    async Loaded(c) { c.listMode = 'pass'; await settle(c.listHeld, 'pass'); },
    async LoadFail(c) { c.listMode = 'fail'; await settle(c.listHeld, 'fail'); },
    async Retry(c) {
      c.listMode = 'hold';
      await click(c, sidebar(c).locator('.side-fresh').getByRole('button', { name: 'Retry' }));
    },
    async PollFail(c) { c.listMode = 'fail'; },
    async PollOk(c) { c.listMode = 'pass'; },

    // ---- the server ----
    async Appear(c) {
      c.turn = await newTurn(c);
      const res = await ok(await c.serve.api.post('/api/sessions', { data: { cwd: c.serve.work, prompt: `${A_TITLE}, please` } }), 'create A');
      c.a = (await res.json()).session.id;
      await ok(await c.serve.api.post(`/api/sessions/${c.a}/rename`, { data: { title: A_TITLE } }), 'rename A');
      await waitTaken(c.dir, c.turn);
    },
    async ProjectThread(c) {
      c.b = seed(c.serve, path.join(c.serve.home, 'bsrc'));
      await ok(await c.serve.api.post(`/api/sessions/${c.b}/rename`, { data: { title: 'bravo' } }), 'rename B');
      await ok(await c.serve.api.post(`/api/sessions/${c.b}/project`, { data: { project: c.slug } }), 'file B in p');
    },
    async Finish(c) {
      c.holdAck = true;
      release(c.dir, c.turn);
      c.turn = '';
      await until('A never finished', async () => (await server(c)).a === 'done');
    },
    async Fail(c) {
      releaseWith(c.dir, c.turn, { mode: 'error', error: 'model says no' });
      c.turn = '';
      await until('A never failed', async () => (await server(c)).a === 'failed');
    },
    async AutoAck(c) {
      await until('the page never acked the finish it shows', () => c.acks.length > 0);
      c.holdAck = false;
      await settle(c.acks, 'pass');
    },

    // ---- rows ----
    OpenRow: async (c) => click(c, await aRow(c)),
    async Back(c) {
      // A page's own Back and the phone thread's; a desktop's thread hides
      // its Back, and there it is the sidebar's history arrow.
      const own = c.page.locator('.app-main button.back');
      if (await own.isVisible()) await click(c, own);
      else await click(c, sidebar(c).locator('.side-bar').getByRole('button', { name: 'Back', exact: true }));
    },
    NavPage: (c) => click(c, sidebar(c).getByRole('navigation', { name: 'Views' }).getByRole('link', { name: 'Hooks' })),
    Hover: async (c) => (await aRow(c)).hover(),
    Unhover: (c) => park(c),
    // A desktop reveals Seen while the pointer is on the row (or focus is
    // in it); touch shows it always. So the pointer goes to the row first,
    // as a hand does, and then to Seen.
    async MarkSeen(c) {
      const seen = sidebar(c).getByRole('button', { name: `Mark ${A_TITLE} seen` }).first();
      await seen.locator('xpath=..').locator('button.row').hover();
      await click(c, seen);
    },
    FoldGroup: (c) => click(c, folderHead(c)),

    // ---- drag and drop ----
    async DragStart(c) {
      const box = (await (await aRow(c)).boundingBox())!;
      await c.page.mouse.move(box.x + 30, box.y + box.height / 2);
      await c.page.mouse.down();
      await c.page.mouse.move(box.x + 60, box.y + box.height / 2 + 4, { steps: 6 });
      await until('the drag never started', () => c.page.evaluate(() => (window as unknown as { __sbDrag?: boolean }).__sbDrag === true));
    },
    async DragOver(c) {
      const box = (await projectGroup(c).boundingBox())!;
      const x = box.x + box.width / 2, y = box.y + box.height / 2;
      await c.page.mouse.move(x, y, { steps: 6 });
      // Blink sends dragenter, not dragover, on the move that enters an
      // element: a pointer that stops there has never been dragged over
      // it. Two more moves inside it are what a hand does.
      await c.page.mouse.move(x + 2, y);
      await c.page.mouse.move(x + 4, y);
    },
    async DragLeave(c) {
      const box = (await groupRow(c).boundingBox())!;
      await c.page.mouse.move(box.x + 60, box.y + box.height / 2, { steps: 6 });
    },
    // Let go over A's own group, which takes no drop.
    async DragCancel(c) {
      const box = (await groupRow(c).boundingBox())!;
      await c.page.mouse.move(box.x + 60, box.y + box.height / 2, { steps: 6 });
      await c.page.mouse.up();
      await park(c);
    },
    async Drop(c) {
      await c.page.mouse.up();
      await park(c);
      await until('the drop sent no move', () => c.assign !== null);
    },
    async MoveOk(c) {
      const r = c.assign!;
      c.assign = null;
      await go(r);
    },
    async MoveFail(c) {
      const r = c.assign!;
      c.assign = null;
      await refuse(r, 409);
    },

    // ---- collapse ----
    ToggleSide: (c) => click(c, sidebar(c).locator('.side-collapse')),
    RailFilter: (c) => click(c, c.page.locator('.sidebar-closed').getByRole('button', { name: 'Filter the list' })),
    async Resize(c) {
      const phone = c.page.viewportSize()!.width <= 720;
      await c.page.setViewportSize(phone ? DESKTOP : PHONE);
    },

    // ---- the filter ----
    Slash: (c) => c.page.keyboard.press('/'),
    FilterButton: (c) => click(c, sidebar(c).locator('.side-bar button[aria-controls="q"]')),
    TypeHit: (c) => field(c).fill(HIT),
    TypeMiss: (c) => field(c).fill(MISS),
    ClearQuery: (c) => field(c).fill(''),
    Escape: (c) => field(c).press('Escape'),

    // ---- Archived ----
    async ArchivedHead(c) {
      const head = archHead(c);
      // Off (no count yet): this opens it, and its read is held until
      // ArchivedLoaded or ArchivedFail, however the last one was answered.
      // Opened from "Include", under a query, the page ties it to that query.
      if (!(await head.locator('.sec-count').count())) {
        c.archMode = 'hold';
        c.included = /Include/.test((await head.textContent()) ?? '');
      }
      await click(c, head);
    },
    async ArchivedLoaded(c) { c.archMode = 'pass'; await until('no Archived read', () => c.archHeld.length > 0); await settle(c.archHeld, 'pass'); },
    async ArchivedFail(c) { c.archMode = 'fail'; await until('no Archived read', () => c.archHeld.length > 0); await settle(c.archHeld, 'fail'); },
    async ArchivedRetry(c) {
      c.archMode = 'hold';
      await click(c, c.page.locator('#sec-archived').getByRole('button', { name: 'Retry' }));
    },
  },

  read: readUiState,
  status: (c) => (c.carrier === 'main' ? c.page.locator('.app-main') : sidebar(c)),
  async invariants(c, where) {
    // A's row, wherever it is drawn, says what serve says of it.
    const r = await aRow(c);
    if (c.a && await r.isVisible()) {
      const label = (await r.getAttribute('aria-label')) ?? '';
      const word = label.split(', ')[1] ?? '';
      const { a } = await server(c);
      const want = a === 'running' ? /^Running$/ : a === 'done' ? /^Done$/ : /^Failed/;
      expect(word, `${where}: A's row says "${label}" while serve has it ${a}`).toMatch(want);
    }
    // The saved fold and the drawn one agree wherever the group is drawn.
    const head = folderHead(c);
    if (await head.isVisible()) {
      const saved = await c.page.evaluate((work) => {
        try { return (JSON.parse(localStorage.getItem('bough:ws-folded') ?? '[]') as string[]).includes(`recent:${work}`); } catch { return false; }
      }, c.serve.work);
      expect(await head.getAttribute('aria-expanded'), `${where}: folder group drawn against its saved fold`).toBe(saved ? 'false' : 'true');
    }
    await sidebarAxe(c, where);
    await uiInvariants(c.page, {
      include: ['#row-card', '.toast'],
      text: '.side-fresh, .list-none, .needs .eyebrow, .ws-name, .ws-lifted, .ws-drop-hint, .sec-fold, .row-title, .row-card-title, .side-title',
    }, where);
  },
  sessions: () => [],
  async cleanup(c) {
    await settle(c.listHeld, 'pass');
    await settle(c.archHeld, 'pass');
    await settle(c.acks, 'pass');
    if (c.assign) await c.assign.abort().catch(() => {});
    c.assign = null;
    if (c.turn) release(c.dir, c.turn);
    // A walk that timed out has lost its page already.
    await c.page.unrouteAll({ behavior: 'ignoreErrors' }).catch(() => {});
  },
});
