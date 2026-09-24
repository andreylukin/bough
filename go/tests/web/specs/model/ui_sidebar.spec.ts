// go/tests/model/specs/ui_sidebar.fizz walked in the browser (recipe:
// go/tests/model/README.md): the control room's sidebar as the person
// meets it — the list read, the filter, Archived, the rail, the hover
// card, Seen, the unseen dot, Needs you, a group's fold, and dragging a
// session onto a project. Every generated path is one test against its
// own serve (llm-control is the model); at every node readUiState must
// equal the spec's Sidebar#0 state.
//
// The world is the spec's: session A (created by Appear, one held turn
// that Finish or Fail ends), project p whose thread B ProjectThread
// starts (a session filed into p by another client), and Z, archived
// before the page loads. The walk decides when the server answers:
//
//   - list reads (GET /api/sessions) are held while the spec's list is
//     loading, failed while it says unavailable or delayed, and let
//     through otherwise; Archived's reads (?all=1) the same for its own
//     loading and failure. A failed read is a body that is not JSON:
//     Chromium logs every 5xx as a console error.
//   - a session's ack is held until AutoAck (the page's own, on viewing
//     an unseen finish) or answered at once for Seen;
//   - the move's POST assign is held until MoveOk or MoveFail.
//
// The page's clock is the walk's (pollStepMs 0): a server step ends with
// the list poll that shows it, moved on by the step itself, so nothing
// else the page times (the hover card, Archived's pending hint) moves
// behind the walk's back.
//
// What the page does not draw at a node (A's row on the rail, or behind
// a filter that misses it) is carried from the steps that set it, the
// way the Go adapters track the client's own state; wherever the page
// does draw it, it is read from the DOM.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

const DESKTOP = { width: 1100, height: 700 };
const PHONE = { width: 600, height: 700 };
const HIT = 'alpha';
const MISS = 'zzqx';
// The list poll: every 4 s, every 12 s while a session is open.
const POLL_MS = 12_500;

test.describe.configure({ timeout: 180_000 });
test.use({ actionTimeout: 10_000 });

type Mode = 'hold' | 'fail' | 'pass';

interface Mem {
  a: string; unseen: boolean; trouble: boolean; project: string; proj: boolean;
  rows: string; search: string; move: string; archFromFilter: boolean; arch: string; groupFolded: boolean;
}

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;        // llm-control's queue
  n: number;
  a: string; b: string; z: string; // session ids ('' until made)
  turn: string;       // A's held turn
  listMode: Mode; listHeld: Route[];
  archMode: Mode; archHeld: Route[];
  acks: Route[];
  assign: Route | null;
  mem: Mem;
}

const name = (c: Ctx, p: string) => `${p}${String(++c.n).padStart(4, '0')}`;

async function until<T>(what: string, fn: () => Promise<T | undefined> | T | undefined, ms = 15_000): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v !== undefined && v !== false) return v as T;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

const fail = (r: Route) => r.fulfill({ status: 200, contentType: 'application/json', body: 'list unavailable' }).catch(() => {});

async function apiStatus(c: Ctx, id: string): Promise<string> {
  const res = await c.serve.api.get(`/api/sessions/${id}`);
  if (!res.ok()) throw new Error(`session: ${res.status()}`);
  return (await res.json()).session.status;
}

const isList = (u: string) => new URL(u).pathname === '/api/sessions';

// The list read that shows a server step: the page's own poll, its clock
// moved past the interval, answered as the mode says.
async function poll(c: Ctx): Promise<void> {
  const answered = c.page.waitForResponse((res) => res.request().method() === 'GET' && isList(res.url()), { timeout: 10_000 });
  await c.page.clock.fastForward(POLL_MS);
  await answered;
  await settle(c);
}

// Lets React commit what a read or a click changed.
async function settle(c: Ctx): Promise<void> {
  await c.page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
}

// A's row as drawn: its pin under Needs you, else its row in a group.
const aRow = (c: Ctx) => c.page.locator(`.sidebar .needs button.row[data-id="${c.a}"], .sidebar [role="tree"] > .ws button.row[data-id="${c.a}"]`).first();
const neutral = async (c: Ctx) => {
  // The thread pane's top right: no row, no drop target.
  const vp = c.page.viewportSize()!;
  await c.page.mouse.move(vp.width - 20, 20);
};

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const m = c.mem;
  const dom = await c.page.evaluate(({ a, hit, miss }) => {
    const q = (sel: string) => document.querySelector(sel) as HTMLElement | null;
    const phone = window.innerWidth <= 720;
    const app = q('.app');
    const sidebar = q('.sidebar');
    const shown = !!sidebar && getComputedStyle(sidebar).display !== 'none' && sidebar.getBoundingClientRect().width > 0;
    const side = !shown ? 'hidden' : sidebar!.classList.contains('sidebar-closed') ? 'rail' : 'full';
    const hash = location.hash;
    const route = hash === '' || hash === '#/' ? 'home' : a && hash === `#/s/${a}` ? 'session' : hash === '#/hooks' ? 'page' : `other: ${hash}`;
    const collapse = q('.side-collapse');
    const field = q('#q') as HTMLInputElement | null;
    const value = field?.value ?? null;
    const search = value === null ? null : value === '' ? 'open' : value === hit ? 'hit' : value === miss ? 'miss' : `other: ${value}`;
    const fresh = q('.sidebar .side-fresh')?.textContent ?? '';
    const tree = q('.sidebar [role="tree"]');
    const none = [...(tree?.querySelectorAll('.list-none') ?? [])].map((e) => e.textContent ?? '').join(' | ');
    // Archived: its section's fold, count and body.
    const arch = [...document.querySelectorAll('.sidebar .sec')].find((s) => s.querySelector('button.sec-fold[aria-controls="sec-archived"]')) as HTMLElement | undefined;
    const archFold = arch?.querySelector('button.sec-fold');
    const archOpen = archFold?.getAttribute('aria-expanded') === 'true';
    const archCount = !!arch?.querySelector('.sec-count');
    const archBody = arch?.querySelector('.sec-body')?.textContent ?? '';
    // A's row: a pin, a row in a group (not Archived's), or under a folded "work" group.
    const pin = a ? q(`.sidebar .needs button.row[data-id="${a}"]`) : null;
    const inGroup = a ? q(`.sidebar [role="tree"] > .ws button.row[data-id="${a}"]`) : null;
    const row = pin ?? inGroup;
    const heads = [...document.querySelectorAll('.sidebar [role="tree"] > .ws > button.ws-head')] as HTMLElement[];
    const work = heads.find((h) => !h.dataset.project);
    const pHead = heads.find((h) => h.dataset.project === 'p');
    const inP = !!(a && pHead && pHead.parentElement?.querySelector(`button.row[data-id="${a}"]`));
    return {
      phone, pane: app?.getAttribute('data-pane') ?? '', side, route,
      closed: collapse?.getAttribute('aria-expanded') === 'false',
      search, inField: document.activeElement?.id === 'q',
      fresh, none,
      archFound: !!arch, archOpen, archCount, archBody,
      pinned: !!pin, grouped: !!inGroup,
      workFolded: work?.getAttribute('aria-expanded') === 'false',
      workDrawn: !!work,
      label: row?.getAttribute('aria-label') ?? '',
      bad: !!row?.querySelector('.row-meta-bad') || !!row?.querySelector('.row-mark svg[stroke="var(--red)"]'),
      dot: !!row?.querySelector('.unseen-dot'),
      // Seen: on the pin, or on the group's copy (a project's thread
      // keeps its row in its group, and the pin then carries none).
      seenBtn: [pin, inGroup].some((r) => !!r?.parentElement?.querySelector('button.row-ack')),
      // What the list holds: a group, a pin, or Archived's rows (its count).
      content: !!tree?.querySelector(':scope > .ws, :scope > .needs') || Number(arch?.querySelector('.sec-count')?.textContent?.replace(/\D/g, '') || 0) > 0,
      pGroup: !!pHead, inP,
      dropOk: !!q('.sidebar .ws[data-drop]'), dropOver: !!q('.sidebar .ws[data-drop="over"]'),
      card: !!q('#row-card'),
    };
  }, { a: c.a, hit: HIT, miss: MISS });

  const full = dom.side === 'full';
  // The list read, as the list says it; the rail and a phone's thread do not.
  let rows = m.rows;
  if (full) {
    rows = dom.fresh.startsWith('Sessions unavailable') ? 'unavailable'
      : dom.fresh.startsWith('Loading sessions') ? 'loading'
      : dom.fresh.startsWith('Updates delayed') ? 'delayed' : 'ready';
    m.rows = rows;
  }
  // The filter: the field when it is drawn; the rail keeps a query unseen.
  let search = m.search;
  if (dom.side !== 'rail') search = dom.search ?? 'off';
  m.search = search;
  const q = search === 'hit' || search === 'miss';
  // A phone's thread pane keeps the list in the document, hidden.
  const drawn = full && (dom.pinned || dom.grouped);
  // A as its row says it, where drawn.
  if (drawn) {
    const word = dom.label.split(', ')[1] ?? '';
    m.a = word.startsWith('Running') ? 'running' : word.startsWith('Done') ? 'done' : dom.bad ? 'failed' : `unknown: ${dom.label}`;
    m.unseen = dom.label.includes('not seen yet');
    m.trouble = dom.seenBtn;
    if (full && !q) m.project = dom.inP ? 'p' : '';
  }
  if (full && (rows === 'ready' || rows === 'delayed') && !q) m.proj = dom.pGroup;
  // A group left empty is not drawn (A pinned under Needs you): its fold
  // is kept, unseen, for when it has a row again.
  if (full && dom.workDrawn) m.groupFolded = dom.workFolded;
  // Archived, as its section says, while the list is on screen.
  let arch = m.arch;
  if (full && dom.archFound) {
    arch = dom.archOpen
      ? (/Couldn’t load archived/.test(dom.archBody) ? 'failed' : /Loading archived/.test(dom.archBody) ? 'loading' : dom.archCount ? 'open' : `unknown: ${dom.archBody}`)
      : dom.archCount ? 'folded' : 'off';
    m.arch = arch;
  }
  // Dragging: the drop targets the page offers; with no project group
  // there is none to see, and the drag is the walk's own.
  let move = m.move;
  if (dom.dropOver) move = 'over';
  else if (dom.dropOk) move = 'dragging';
  const says = !full ? dom.side
    : rows !== 'ready' ? rows
    : dom.content ? 'rows'
    : /Loading archived|Couldn’t load archived|taking too long/.test(dom.archBody) ? 'archived'
    : /No sessions match/.test(dom.none) ? 'nomatch'
    : /No sessions yet/.test(dom.none) ? 'empty' : `unknown: ${dom.none}`;
  const aRowAt = !full ? 'none' : dom.pinned ? 'pinned' : dom.grouped ? 'group' : m.a !== 'none' && dom.workFolded && !q ? 'folded' : 'none';
  return {
    viewport: dom.phone ? 'phone' : 'desktop',
    pane: dom.pane,
    route: dom.route,
    closed: dom.closed,
    search,
    inField: dom.inField,
    rows,
    a: m.a,
    unseen: m.unseen,
    trouble: m.trouble,
    project: m.project,
    proj: m.proj,
    groupFolded: m.groupFolded,
    arch,
    archFromFilter: m.archFromFilter,
    move,
    card: dom.card,
    side: dom.side,
    says,
    aRow: aRowAt,
    dot: full && dom.dot,
    seenBtn: full && dom.seenBtn,
  };
}

// The query cleared: Archived included from it goes with it, and a read
// of it still held is one the page no longer waits for.
async function queryCleared(c: Ctx): Promise<void> {
  if (c.mem.archFromFilter) {
    c.mem.archFromFilter = false;
    c.archMode = 'pass';
    for (const r of c.archHeld.splice(0)) await r.continue().catch(() => {});
  }
  await settle(c);
}

modelTests<Ctx>({
  spec: 'ui_sidebar',
  role: 'Sidebar#0',
  config: CONTROL_CONFIG,
  pollStepMs: 0,
  // A refused move (MoveFail) is a 409 the page shows in its error bar.
  allowConsole: /^Failed to load resource: the server responded with a status of 409 /,

  async init(page, serve) {
    // Project p, and Z archived before the page ever reads the list.
    fs.mkdirSync(path.join(serve.home, '.bough', 'projects', 'p'), { recursive: true });
    fs.writeFileSync(path.join(serve.home, '.bough', 'projects', 'p', 'project.yml'), 'name: p\nrepos: []\n');
    const z = await serve.newSession();
    const res = await serve.api.post(`/api/sessions/${z}/archive`, { data: {} });
    if (!res.ok()) throw new Error(`archive: ${res.status()}`);
    const c: Ctx = {
      page, serve, dir: controlDir(serve.home), n: 0, a: '', b: '', z, turn: '',
      listMode: 'hold', listHeld: [], archMode: 'pass', archHeld: [], acks: [], assign: null,
      mem: { a: 'none', unseen: false, trouble: false, project: '', proj: false, rows: 'loading', search: 'off', move: 'none', archFromFilter: false, arch: 'off', groupFolded: false },
    };
    fs.mkdirSync(c.dir, { recursive: true });
    await page.setViewportSize(DESKTOP);
    // The first-run welcome (setup_welcome's) takes the thread pane while
    // no session is listed; this world starts with only Z, archived.
    await page.addInitScript(() => { try { localStorage.setItem('bough:welcome-done', '1'); } catch { /* storage off */ } });
    await page.route((u) => u.pathname === '/api/sessions', (r) => {
      if (r.request().method() !== 'GET') return r.continue();
      const all = new URL(r.request().url()).searchParams.has('all');
      const mode = all && c.archMode !== 'pass' ? c.archMode : c.listMode;
      if (mode === 'hold') { (all && c.archMode === 'hold' ? c.archHeld : c.listHeld).push(r); return; }
      if (mode === 'fail') return fail(r);
      return r.continue();
    });
    await page.route((u) => /^\/api\/sessions\/[^/]+\/ack$/.test(u.pathname), (r) => { c.acks.push(r); });
    await page.route((u) => /^\/api\/sessions\/[^/]+\/project$/.test(u.pathname), (r) => {
      if (r.request().method() !== 'POST') return r.continue();
      c.assign = r;
    });
    await page.goto(`${serve.url}/#/`);
    await until('the first list read', () => c.listHeld.length > 0 || undefined);
    // Past the 200 ms before a first read says it is loading.
    await page.clock.fastForward(300);
    return c;
  },

  actions: {
    // --- the list read
    async Loaded(c) {
      c.listMode = 'pass';
      const answered = c.page.waitForResponse((res) => isList(res.url()));
      for (const r of c.listHeld.splice(0)) await r.continue();
      await answered;
      await settle(c);
    },
    async LoadFail(c) {
      c.listMode = 'fail';
      for (const r of c.listHeld.splice(0)) await fail(r);
      await settle(c);
    },
    async Retry(c) {
      c.listMode = 'hold';
      await c.page.locator('.sidebar .side-fresh').getByRole('button', { name: 'Retry' }).click();
      await until('the list read', () => c.listHeld.length > 0 || undefined);
    },
    async PollFail(c) {
      c.listMode = 'fail';
      await poll(c);
    },
    async PollOk(c) {
      c.listMode = 'pass';
      await poll(c);
    },

    // --- the server
    async Appear(c) {
      c.turn = name(c, 'a');
      queue(c.dir, c.turn, { mode: 'block' });
      c.a = await c.serve.newSession('alpha task');
      await waitTaken(c.dir, c.turn);
      await until('A running', async () => (await apiStatus(c, c.a)) === 'running' || undefined);
      c.mem.a = 'running';
      await poll(c);
    },
    async ProjectThread(c) {
      c.b = await c.serve.newSession();
      const res = await c.serve.api.post(`/api/sessions/${c.b}/project`, { data: { project: 'p' } });
      if (!res.ok()) throw new Error(`file B into p: ${res.status()}`);
      c.mem.proj = true;
      await poll(c);
    },
    async Finish(c) {
      releaseWith(c.dir, c.turn, { mode: 'ok', text: 'alpha done' });
      await until('A done', async () => (await apiStatus(c, c.a)) !== 'running' || undefined);
      c.mem.a = 'done';
      c.mem.unseen = true;
      await poll(c);
    },
    async Fail(c) {
      releaseWith(c.dir, c.turn, { mode: 'error', error: 'model says no' });
      await until('A failed', async () => (await apiStatus(c, c.a)) !== 'running' || undefined);
      c.mem.a = 'failed';
      c.mem.trouble = true;
      await poll(c);
    },
    // The page's own ack of the finish on screen: let it through.
    async AutoAck(c) {
      await until('the page to ack', () => c.acks.length > 0 || undefined, 10_000);
      const refreshed = c.page.waitForResponse((res) => isList(res.url()));
      for (const r of c.acks.splice(0)) await r.continue();
      await refreshed;
      c.mem.unseen = false;
      await settle(c);
    },

    // --- rows
    async OpenRow(c) {
      await aRow(c).click();
      await neutral(c);
      await settle(c);
    },
    async Back(c) {
      const hash = await c.page.evaluate(() => location.hash);
      const phone = await c.page.evaluate(() => window.innerWidth <= 720);
      const own = c.page.locator('main button.back:visible');
      if ((hash === '#/hooks' || phone) && await own.count()) await own.first().click();
      else await c.page.locator('.sidebar .side-bar').getByRole('button', { name: 'Back', exact: true }).click();
      await until('home', async () => ['', '#/'].includes(await c.page.evaluate(() => location.hash)) || undefined);
      await neutral(c);
      await settle(c);
    },
    async NavPage(c) {
      await c.page.locator('.sidebar nav').getByRole('link', { name: 'Hooks' }).click();
      await neutral(c);
      await settle(c);
    },
    // The pointer travels to the row, as a hand moves it: a jump in one
    // event after a drag ended lands on the element Chromium still holds
    // as hovered from before the drag, and enters nothing.
    async Hover(c) {
      const box = (await aRow(c).boundingBox())!;
      await c.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 8 });
      await c.page.clock.fastForward(400);
      await settle(c);
    },
    async Unhover(c) {
      await neutral(c);
      await settle(c);
    },
    async MarkSeen(c) {
      const refreshed = c.page.waitForResponse((res) => isList(res.url()));
      // Seen shows on the row's hover (or focus within it): the pointer
      // rests on the row first, as a person's does on the way to it.
      const wrap = c.page.locator(`.sidebar .row-wrap:has(button.row[data-id="${c.a}"]):has(button.row-ack)`).first();
      await wrap.hover();
      await wrap.locator('button.row-ack').click();
      await until('the ack', () => c.acks.length > 0 || undefined);
      for (const r of c.acks.splice(0)) await r.continue();
      await refreshed;
      c.mem.trouble = false;
      await neutral(c);
      await settle(c);
    },
    async FoldGroup(c) {
      await c.page.locator('.sidebar [role="tree"] > .ws > button.ws-head:not([data-project])').click();
      await neutral(c);
      await settle(c);
    },

    // --- drag and drop, with the mouse as a person drags
    async DragStart(c) {
      const box = (await aRow(c).boundingBox())!;
      await c.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await c.page.mouse.down();
      await c.page.mouse.move(box.x + box.width / 2 + 10, box.y + box.height / 2 + 4, { steps: 3 });
      c.mem.move = 'dragging';
      await settle(c);
    },
    async DragOver(c) {
      const box = (await c.page.locator('.sidebar [role="tree"] > .ws > button.ws-head[data-project="p"]').boundingBox())!;
      await c.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 4 });
      c.mem.move = 'over';
      await settle(c);
    },
    async DragLeave(c) {
      const vp = c.page.viewportSize()!;
      await c.page.mouse.move(vp.width - 20, 20, { steps: 4 });
      c.mem.move = 'dragging';
      await settle(c);
    },
    async DragCancel(c) {
      const vp = c.page.viewportSize()!;
      await c.page.mouse.move(vp.width - 20, 20, { steps: 4 });
      await c.page.mouse.up();
      c.mem.move = 'none';
      await settle(c);
    },
    async Drop(c) {
      await c.page.mouse.up();
      await until('POST assign', () => c.assign !== null || undefined);
      c.mem.move = 'pending';
      await neutral(c);
      await settle(c);
    },
    async MoveOk(c) {
      const refreshed = c.page.waitForResponse((res) => isList(res.url()));
      await c.assign!.continue();
      c.assign = null;
      await refreshed;
      c.mem.move = 'none';
      await settle(c);
    },
    async MoveFail(c) {
      await c.assign!.fulfill({ status: 409, contentType: 'application/json', body: JSON.stringify({ error: 'serve: api: the move was refused' }) });
      c.assign = null;
      c.mem.move = 'none';
      await settle(c);
    },

    // --- collapse
    // The spec's pointer is on A's row only between Hover and Unhover,
    // and its fold takes the card away for good: the hand is on the keys.
    // Left resting where the row comes back, it would hover it again.
    async ToggleSide(c) {
      await neutral(c);
      await c.page.keyboard.press('ControlOrMeta+b');
      await settle(c);
    },
    async RailFilter(c) {
      await c.page.locator('.sidebar-closed').getByRole('button', { name: 'Filter the list' }).click();
      await settle(c);
    },
    async Resize(c) {
      const phone = await c.page.evaluate(() => window.innerWidth <= 720);
      await c.page.setViewportSize(phone ? DESKTOP : PHONE);
      await settle(c);
    },

    // --- the filter
    async Slash(c) {
      await c.page.keyboard.press('/');
      await settle(c);
    },
    async FilterButton(c) {
      const clears = c.mem.search !== 'off';
      await c.page.locator('.sidebar .side-bar').getByRole('button', { name: 'Filter the list' }).click();
      await neutral(c);
      if (clears) await queryCleared(c);
      else await settle(c);
    },
    async TypeHit(c) {
      await c.page.locator('#q').fill(HIT);
      await settle(c);
    },
    async TypeMiss(c) {
      await c.page.locator('#q').fill(MISS);
      await settle(c);
    },
    async ClearQuery(c) {
      await c.page.locator('#q').fill('');
      await queryCleared(c);
    },
    async Escape(c) {
      await c.page.keyboard.press('Escape');
      await queryCleared(c);
    },

    // --- Archived
    async ArchivedHead(c) {
      const off = c.mem.arch === 'off';
      if (off) {
        c.archMode = 'hold';
        c.mem.archFromFilter = c.mem.search === 'hit' || c.mem.search === 'miss';
      }
      await c.page.locator('.sidebar button.sec-fold[aria-controls="sec-archived"]').click();
      if (off) await until('the Archived read', () => c.archHeld.length > 0 || undefined);
      await neutral(c);
      await settle(c);
    },
    async ArchivedLoaded(c) {
      c.archMode = 'pass';
      const answered = c.page.waitForResponse((res) => isList(res.url()));
      for (const r of c.archHeld.splice(0)) await r.continue();
      await answered;
      await settle(c);
    },
    async ArchivedFail(c) {
      c.archMode = 'fail';
      for (const r of c.archHeld.splice(0)) await fail(r);
      await settle(c);
    },
    async ArchivedRetry(c) {
      c.archMode = 'hold';
      await c.page.locator('.sidebar .sec-body .inline-fail').getByRole('button', { name: 'Retry' }).click();
      await until('the Archived read', () => c.archHeld.length > 0 || undefined);
      await settle(c);
    },
  },

  read: readUiState,
  // The sidebar's toolbar is on screen at every node but a phone's thread,
  // whose own header is then.
  status: (c) => c.page.locator('.sidebar .side-bar:visible, main .thread-head:visible, main .page-head:visible, main h1:visible').first(),
  sessions: (c) => (c.a ? [c.a] : []),
  async cleanup(c) {
    if (!c) return;
    c.listMode = 'pass';
    c.archMode = 'pass';
    for (const r of [...c.listHeld.splice(0), ...c.archHeld.splice(0), ...c.acks.splice(0)]) await r.continue().catch(() => {});
    if (c.assign) await c.assign.continue().catch(() => {});
    await c.page.mouse.up().catch(() => {});
    if (c.turn) releaseWith(c.dir, c.turn, { mode: 'ok', text: 'cleanup' });
  },
});
