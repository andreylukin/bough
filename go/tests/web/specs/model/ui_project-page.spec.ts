// go/tests/model/specs/ui_project-page.fizz walked in the browser (recipe:
// go/tests/model/README.md): one project's page (#/projects/<slug>) as
// the person sees it: the first read, the home's composer, main and the
// one thread beside the thread column (folded to a rail, or a drawer at
// tight), the Project panel with the MEMORY.md editor and main's orb
// strip, and the Stop orb confirm. Every generated walk is one test; at
// each node readUiState must equal the spec's Page#0 state, and the page
// owes the shared surface checks (helpers/ui-invariants.ts) on top of
// the harness's own.
//
// The serve is real: llm-control as the model (a thread's turn is held
// until ThreadFinish), the orb row on the fake runtime, so main and the
// thread are real project sessions whose transcripts the page shows.
// What the walk decides is when the server answers, and three facts the
// fake runtime cannot be told:
//
// - The page's own requests are held at the network (page.route) until
//   the spec's answer: the first reads of the detail and the files
//   (Loaded/LoadFail, FilesLoaded/FilesFail), a message (SendOk/
//   SendFail) and a MEMORY.md save (SaveOk/SaveFail). A failure is
//   answered by the walk as serve answers one ({"error": ...}); while the
//   spec says the project cannot be read, every read of it fails.
// - main's orb (the spec's `orb`): the fake runtime runs a container at
//   once and never stops one on its own, so `mainOrb` in the detail the
//   page reads is the walk's (starting after a message, running at
//   OrbUp, stopped at OrbExit or on the page's Stop), as serve would
//   report the runtime's.
// - main's running jobs: the spec's Stop asks first because main has
//   jobs running; the session list the page reads says main has one.
//
// The page's clock runs in real time (axe needs it) and the harness does
// not move it before a read (pollStepMs 0): the "MEMORY.md saved." note
// lasts 2.6 s, and a read that jumped the clock would fade it. A step
// the page learns of by its 4 s poll moves the clock past the poll
// itself.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { queue, release } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';
import { uiInvariants } from '../../helpers/ui-invariants';

const CONFIG = '- id: llm\n  plugin: llm-control\n' +
  '- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n';

// The viewport either side of the page's 1080px class.
const WIDE = { width: 1100, height: 700 };
const TIGHT = { width: 1000, height: 700 };

const WAIT_MS = 20_000;
// The project page's poll, and the App's session list's while a session
// is open (it is what acks a finish the person is looking at).
const POLL_MS = 4_100;
const LIST_MS = 12_500;

// Up to 51 nodes a walk, each with an axe pass.
test.describe.configure({ timeout: 600_000 });
// A control the page does not offer fails its step, not the whole walk's time.
test.use({ actionTimeout: 20_000 });

type Net = 'loading' | 'error' | 'loaded';

interface Detail {
  main?: string;
  mainOrb?: { status: string };
  threads: { id: string; status: string; unseen?: boolean; empty?: boolean }[];
}

interface Ctx {
  page: Page;
  serve: Serve;
  slug: string;
  tag: string;
  dir: string;          // llm-control's queue
  // What serve answers, as the walk decides it.
  detail: Net;
  files: Net;
  orb: string;          // main's orb: none, starting, running, stopped
  held: { detail: Route[]; files: Route[]; message: Route[]; save: Route[] };
  last?: Detail;        // the detail the page was last answered with
  main: string;
  thread: string;
  turn: string;         // the thread's held turn
  turns: number;
  msgs: number;
  mainIn: number;       // inputs main has been given: messages and reports
  edits: number;
  lastFold: boolean;    // the column's Done head, as the page last showed it
}

let seq = 0;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until(what: string, f: () => boolean | Promise<boolean>): Promise<void> {
  const deadline = Date.now() + WAIT_MS;
  while (!(await f())) {
    if (Date.now() > deadline) throw new Error(`ui_project-page: waiting for ${what}`);
    await sleep(20);
  }
}

// --- serve's transcripts: only to keep the one model queue in order

interface Entry { kind: string; data?: Record<string, unknown> }

function entries(c: Ctx, id: string): Entry[] {
  try {
    return fs.readFileSync(path.join(c.serve.home, '.bough', 'history', id + '.jsonl'), 'utf8')
      .split('\n').filter(Boolean).map((l) => JSON.parse(l) as Entry);
  } catch { return []; }
}

function turnOpen(es: Entry[]): boolean {
  let open = false;
  for (const e of es) {
    if (e.kind === 'input') open = true;
    else if (e.kind === 'done' || e.kind === 'cancelled') open = false;
  }
  return open;
}

/**
 * main has taken n inputs and answered them all. Every session takes
 * llm-control's next queued turn, so main must be quiet before a
 * thread's turn is queued, or main would take it.
 */
const mainQuiet = (c: Ctx, n: number) => until(`main ${c.main} to answer ${n} inputs`, () => {
  const es = entries(c, c.main);
  return es.filter((e) => e.kind === 'input').length >= n && !turnOpen(es);
});

// --- the network

const err = (r: Route, status: number, msg: string) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify({ error: msg }) });

/** The detail from serve, with main's orb as the walk has it. */
async function answerDetail(c: Ctx, r: Route): Promise<void> {
  let res;
  try { res = await r.fetch(); } catch { return; } // the page went away
  const d = (await res.json()) as Detail & Record<string, unknown>;
  if (c.orb === 'none') delete d.mainOrb;
  else d.mainOrb = { ...(d.mainOrb ?? {}), session: d.main, project: c.slug, status: c.orb, updatedAt: new Date().toISOString(), up: c.orb === 'running' } as Detail['mainOrb'];
  c.last = d;
  await r.fulfill({ response: res, json: d }).catch(() => undefined);
}

async function onRoute(c: Ctx, r: Route): Promise<void> {
  const req = r.request();
  const p = new URL(req.url()).pathname;
  const base = `/api/projects/${c.slug}`;
  if (p === base && req.method() === 'GET') {
    if (c.detail === 'loading') c.held.detail.push(r);
    else if (c.detail === 'error') await err(r, 500, 'serve: the project could not be read');
    else await answerDetail(c, r);
  } else if (p === `${base}/orb` && req.method() === 'GET') {
    if (c.files === 'loading') c.held.files.push(r);
    else if (c.files === 'error') await err(r, 500, 'serve: the definition files could not be read');
    else await r.continue();
  } else if (p === `${base}/message` && req.method() === 'POST') {
    c.held.message.push(r);
  } else if (p.startsWith(`${base}/orb/files/`) && req.method() === 'PUT') {
    c.held.save.push(r);
  } else if (c.main && p === `/api/sessions/${c.main}/orb/stop` && req.method() === 'POST') {
    // The runtime the walk plays stops main's container.
    c.orb = 'stopped';
    await r.fulfill({ contentType: 'application/json', body: '{"ok":true}' });
  } else if (p === '/api/sessions' && req.method() === 'GET') {
    let res;
    try { res = await r.fetch(); } catch { return; }
    const body = (await res.json()) as { sessions?: { id: string; jobs?: unknown[] }[] };
    for (const s of body.sessions ?? []) {
      if (s.id === c.main) s.jobs = [{ id: 1, cmd: 'make test', started: new Date().toISOString() }];
    }
    await r.fulfill({ response: res, json: body }).catch(() => undefined);
  } else {
    await r.fallback();
  }
}

/** The next request of kind the page sent, once it has. */
async function next(c: Ctx, kind: keyof Ctx['held']): Promise<Route> {
  await until(`the page's ${kind} request`, () => c.held[kind].length > 0);
  return c.held[kind].shift()!;
}

/**
 * The save the editor on screen is waiting for. A save the panel closed
 * over lands with nobody told (the spec's KNOWN GAPS 2): serve takes it
 * first, so it cannot overwrite the one being answered.
 */
async function lastSave(c: Ctx): Promise<Route> {
  await until("the page's save request", () => c.held.save.length > 0);
  const all = c.held.save.splice(0);
  const last = all.pop()!;
  for (const r of all) await r.fulfill({ response: await r.fetch() });
  return last;
}

/** Moves the page's clock past its poll (and the App's list's, when it is the list that brings the change). */
async function poll(c: Ctx, ms = POLL_MS): Promise<void> {
  await c.page.clock.fastForward(ms);
}

// --- the page

const p = (c: Ctx) => c.page;
const panel = (c: Ctx) => p(c).locator('aside.prj-panel');
const column = (c: Ctx) => p(c).locator('aside.prj-threads:not(.prj-threads-folded)');

/** The one thread's row, on the home or in the column. */
const threadRow = (c: Ctx) => p(c).locator('.prj-home .prj-thread:not(.prj-main-row), aside.prj-threads .prj-thread').first();

const GROUP: Record<string, string> = { Running: 'running', 'Finished — unseen': 'unseen', Done: 'done' };

/** The thread as the last detail the page was given says it, for when no list shows it. */
function threadOf(d?: Detail): string {
  const t = d?.threads ?? [];
  if (t.length === 0) return 'none';
  if (t.length > 1) return `many: ${t.length}`;
  const r = t[0];
  if (r.status === 'running' || r.status === 'queued') return 'running';
  if (r.status === 'done') return r.unseen ? 'unseen' : 'done';
  return `status ${r.status}`;
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await p(c).evaluate(({ slug, PANEL, RAIL }) => {
    const q = (s: string) => document.querySelector<HTMLElement>(s);
    const seen = (el: Element | null) => !!el && (el as HTMLElement).getClientRects().length > 0;
    const store = (k: string) => { try { return localStorage.getItem(k) ?? ''; } catch { return ''; } };
    const wide = !matchMedia('(max-width:1080px)').matches;

    let detail = 'other';
    if (q('.prj h1.prj-name')) detail = 'loaded';
    else if (q('.prj-loading .error-note')?.textContent?.includes('Couldn’t read this project')) detail = 'error';
    else if (q('.prj-loading .pending')?.textContent?.includes('Loading project')) detail = 'loading';

    // The view, and the URL must name the same one: a reload reopens what it says.
    const back = q('.prj-back');
    const crumb = (q('.prj-crumb-rest')?.textContent ?? '').trim();
    let view = detail !== 'loaded' ? 'home' : q('.prj-home') && !back ? 'home' : back ? (crumb === '· Main thread' ? 'main' : 'thread') : 'neither';
    const hash = location.hash;
    const onHash = hash === `#/projects/${slug}` ? 'home' : hash.startsWith(`#/projects/${slug}/t/`) ? 'conversation' : hash;
    if ((view === 'home') !== (onHash === 'home')) view = `${view}, but the URL says ${hash}`;

    const col = q('aside.prj-threads:not(.prj-threads-folded)');
    const home = q('.prj-home');
    const listSel = home ? '.prj-home .prj-queue' : col ? 'aside.prj-threads:not(.prj-threads-folded) .prj-threads-list' : '';

    const doneHead = [...document.querySelectorAll('aside.prj-threads .prj-group-head')]
      .find((h) => h.querySelector('.prj-group-label')?.textContent === 'Done');

    const box = q('.prj-home textarea.prj-first-box') as HTMLTextAreaElement | null;
    let composer = 'empty';
    if (box) {
      const sendBtn = [...document.querySelectorAll('.prj-first button')].find((b) => /^(Send|Sending…)$/.test(b.textContent ?? ''));
      if (sendBtn?.textContent === 'Sending…') composer = box.disabled ? 'sending' : 'sending, box enabled';
      else if (q('.prj-first-err[role=alert]')) composer = box.value.trim() ? 'failed' : 'failed, text lost';
      else composer = box.value.trim() ? 'typed' : 'empty';
    }

    const pan = q('aside.prj-panel');
    let files = '';
    let mem = 'clean';
    let strip = '';
    if (pan) {
      const f = pan.querySelector('.prj-files');
      if (f?.querySelector('textarea.orb-editor')) files = 'loaded';
      else if (f?.querySelector('.pending-err')) files = 'error';
      else if (f?.querySelector('.pending')) files = 'loading';
      else files = 'unseen';
      const ed = pan.querySelector<HTMLTextAreaElement>('textarea.orb-editor');
      if (ed) {
        const tab = pan.querySelector('[role=tab][aria-selected=true]')?.textContent ?? '';
        const save = [...pan.querySelectorAll('.orb-tabs button')].find((b) => /^(Save|Saving…)$/.test(b.textContent ?? '')) as HTMLButtonElement | undefined;
        if (ed.getAttribute('aria-label') !== 'MEMORY.md') mem = `tab ${ed.getAttribute('aria-label')}`;
        else if (save?.textContent === 'Saving…') mem = save.disabled ? 'saving' : 'saving, Save enabled';
        else if (pan.querySelector('.orb-save-err[role=alert]')) mem = 'failed';
        else if (pan.querySelector('.file-said')) mem = tab.includes('•') ? 'saved, still dirty' : 'saved';
        else if (tab.includes('•')) mem = save && !save.disabled ? 'dirty' : 'dirty, Save disabled';
      }
      strip = (pan.querySelector('.prj-orb-line')?.textContent ?? '').trim();
    }

    const dlg = q('[role=dialog]');
    return {
      detail, view, wide,
      pref: store(PANEL),
      railStored: store(RAIL) === '1',
      railShown: !!q('aside.prj-threads-folded'),
      colShown: seen(col),
      drawer: !!q('aside.prj-threads[data-open]'),
      panel: !!pan,
      groups: listSel ? (document.querySelector(listSel) ? [...document.querySelector(listSel)!.querySelectorAll('.prj-group')].map((g) => ({
        label: g.querySelector('.prj-group-label')?.textContent ?? '',
        n: Number(g.querySelector('.prj-group-count')?.textContent ?? 0),
      })) : null) : null,
      mainListed: !!q(home ? '.prj-home .prj-main-row' : '.prj-main-thread'),
      listShown: !!listSel,
      doneFolded: doneHead ? doneHead.getAttribute('aria-expanded') === 'false' : null,
      composer, files, mem, strip,
      dialog: dlg ? (dlg.textContent?.includes('Stop the orb?') ? 'stop' : `other: ${(dlg.textContent ?? '').slice(0, 40)}`) : '',
    };
  }, { slug: c.slug, PANEL: 'bough:prj-panel', RAIL: 'bough:prj-threads-folded' });

  const loaded = dom.detail === 'loaded';

  // Nothing is drawn but the status before the detail lands: the panel
  // is then what the page opens with at this width (up on the home
  // unless it was put away), and the files are what serve answered the
  // page's read with.
  const panelUp = loaded ? dom.panel : dom.wide && (dom.pref ? dom.pref === '1' : true);
  const files = dom.panel && loaded ? dom.files : c.files;

  // The rail is drawn only at wide beside a conversation; elsewhere it is
  // the remembered fold the page will draw it from.
  let rail: unknown = dom.railStored;
  if (loaded && dom.wide && dom.view !== 'home' && dom.railShown !== dom.railStored) rail = `drawn ${dom.railShown}, remembered ${dom.railStored}`;

  // The thread by its group in the list on screen; with none on screen
  // (the rail, a closed drawer), as the last detail the page read says.
  let thread = threadOf(c.last);
  if (dom.groups) {
    const gs = dom.groups.filter((g) => g.n > 0);
    if (gs.length === 0) thread = 'none';
    else if (gs.length > 1 || gs[0].n > 1) thread = `many: ${JSON.stringify(gs)}`;
    else thread = GROUP[gs[0].label] ?? `group ${gs[0].label}`;
  }

  // The Done head folds only in the column; elsewhere it keeps the fold it was last shown with.
  if (dom.doneFolded !== null) c.lastFold = dom.doneFolded;

  // main: its row on the home, pinned in the column, or open.
  let main = Boolean(c.last?.main);
  if (dom.view === 'main' || dom.mainListed) main = true;
  else if (dom.listShown && loaded) main = false;

  // main's orb: the strip's words when the panel shows it. "Stopped" is
  // what it says for a starting orb too (KNOWN GAPS 1 in the spec): the
  // detail it was drawn from tells the two apart.
  const told = c.last?.mainOrb?.status ?? 'none';
  let orb = told;
  if (dom.panel && loaded) {
    if (dom.strip.startsWith('Main thread’s orb —')) orb = 'running';
    else if (dom.strip.startsWith('Main thread’s orb is stopped')) orb = told === 'starting' || told === 'stopped' ? told : `strip stopped, detail ${told}`;
    else if (/^(No orb yet|Main thread has no orb yet)/.test(dom.strip)) orb = 'none';
    else orb = `strip "${dom.strip}"`;
  }

  return {
    detail: dom.detail,
    files,
    main,
    view: dom.view,
    thread,
    done_folded: c.lastFold,
    orb,
    composer: dom.composer,
    wide: dom.wide,
    pref: dom.pref,
    panel: panelUp,
    drawer: dom.drawer,
    rail,
    mem: dom.mem,
    dialog: dom.dialog === 'stop' ? true : dom.dialog ? dom.dialog : false,
  };
}

// --- actions

async function answerAll(rs: Route[], f: (r: Route) => Promise<void>): Promise<void> {
  const all = rs.splice(0);
  await Promise.all(all.map(f));
}

/** A new turn for the thread, held until ThreadFinish. */
function queueTurn(c: Ctx): string {
  const name = `${c.tag}-t${String(++c.turns).padStart(3, '0')}`;
  queue(c.dir, name, { mode: 'block', text: `finished ${name}` });
  return name;
}

async function threadRunning(c: Ctx, name: string): Promise<void> {
  await until(`turn ${name} taken`, () => fs.existsSync(path.join(c.dir, name + '.taken')));
  c.turn = name;
  await until(`thread ${c.thread} in a turn`, () => turnOpen(entries(c, c.thread)));
}

modelTests<Ctx>({
  spec: 'ui_project-page',
  role: 'Page#0',
  config: CONFIG,
  pollStepMs: 0,
  // Chromium logs every failed request: the walk fails the project's
  // read, the files' read, a message and a save on purpose.
  allowConsole: /Failed to load resource: the server responded with a status of (400|500)/,

  async init(page, serve) {
    const tag = `pp${process.pid}x${++seq}`;
    const res = await serve.api.post('/api/projects', { data: { name: `Walk ${tag}` } });
    if (!res.ok()) throw new Error(`new project: ${res.status()} ${await res.text()}`);
    const slug = ((await res.json()) as { project: { slug: string } }).project.slug;
    const c: Ctx = {
      page, serve, slug, tag, dir: path.join(serve.home, '.bough', 'llm-control'),
      detail: 'loading', files: 'loading', orb: 'none',
      held: { detail: [], files: [], message: [], save: [] },
      main: '', thread: '', turn: '', turns: 0, msgs: 0, mainIn: 0, edits: 0, lastFold: true,
    };
    await page.setViewportSize(WIDE);
    await page.route((u) => u.pathname.startsWith('/api/'), (r) => onRoute(c, r));
    await page.goto(`${serve.url}/#/projects/${slug}`);
    // Both first reads are out and held: the page sits on its status.
    await until('the first reads', () => c.held.detail.length > 0 && c.held.files.length > 0);
    return c;
  },

  actions: {
    async Loaded(c) {
      c.detail = 'loaded';
      await answerAll(c.held.detail, (r) => answerDetail(c, r));
    },
    async LoadFail(c) {
      c.detail = 'error';
      await answerAll(c.held.detail, (r) => err(r, 500, 'serve: the project could not be read'));
    },
    async Retry(c) {
      c.detail = 'loading';
      if (c.files === 'error') c.files = 'loading';
      await p(c).locator('.prj-loading .error-note').getByRole('button', { name: 'Retry' }).click();
      await until('the retried read', () => c.held.detail.length > 0);
    },
    async FilesLoaded(c) {
      c.files = 'loaded';
      await answerAll(c.held.files, (r) => r.continue());
    },
    async FilesFail(c) {
      c.files = 'error';
      await answerAll(c.held.files, (r) => err(r, 500, 'serve: the definition files could not be read'));
    },
    async FilesRetry(c) {
      c.files = 'loading';
      await panel(c).locator('.prj-files .pending-err').getByRole('button', { name: 'Retry' }).click();
      await until('the retried files read', () => c.held.files.length > 0);
    },

    async Type(c) {
      const box = p(c).getByRole('textbox', { name: 'Message the project' });
      await box.click();
      await p(c).keyboard.type(`${c.tag} message ${++c.msgs}`);
    },
    async Send(c) {
      await p(c).getByRole('textbox', { name: 'Message the project' }).press('Enter');
      await until('the message', () => c.held.message.length > 0);
    },
    async SendOk(c) {
      const r = await next(c, 'message');
      if (c.orb !== 'running') c.orb = 'starting';
      const res = await r.fetch({ timeout: 60_000 });
      if (!res.ok()) throw new Error(`message: ${res.status()} ${await res.text()}`);
      c.main = ((await res.json()) as { main: string }).main;
      await r.fulfill({ response: res });
      // main answers at once (nothing is queued for it); it must be done
      // before a thread's turn is queued.
      await mainQuiet(c, ++c.mainIn);
    },
    SendFail: async (c) => err(await next(c, 'message'), 500, 'serve: main could not be started'),

    OpenMain: (c) => p(c).locator('.prj-home .prj-main-row, aside.prj-threads .prj-main-thread').first().click(),
    OpenThread: (c) => threadRow(c).click(),
    Back: (c) => p(c).locator('.prj-back').click(),

    FoldRail: (c) => p(c).getByRole('button', { name: 'Hide threads' }).click(),
    UnfoldRail: (c) => p(c).getByRole('button', { name: 'Show threads' }).click(),
    ToggleDone: (c) => column(c).locator('.prj-group-head', { hasText: 'Done' }).click(),
    ToggleDrawer: (c) => p(c).locator('.prj-threads-btn').click(),
    CloseDrawer: (c) => column(c).locator('.prj-drawer-close').click(),

    TogglePanel: (c) => p(c).locator('.prj-panel-btn').click(),
    ClosePanel: (c) => panel(c).locator('.prj-drawer-close').click(),
    async Resize(c) {
      const wide = await p(c).evaluate(() => !matchMedia('(max-width:1080px)').matches);
      await p(c).setViewportSize(wide ? TIGHT : WIDE);
    },

    async Edit(c) {
      await panel(c).getByRole('textbox', { name: 'MEMORY.md' }).click();
      await p(c).keyboard.press('End');
      // Every edit its own words: a save the panel closed over lands later
      // (see lastSave), and a draft equal to it would be no draft at all.
      await p(c).keyboard.type(` ${c.tag}e${++c.edits}`);
    },
    async Save(c) {
      await panel(c).locator('.orb-tabs button', { hasText: /^Save$/ }).click();
      await until('the save', () => c.held.save.length > 0);
    },
    SaveOk: async (c) => (await lastSave(c)).continue(),
    SaveFail: async (c) => err(await lastSave(c), 400, 'MEMORY.md: the walk refused this save'),
    SaidFades: (c) => p(c).clock.fastForward(2_700),

    StopOrb: (c) => panel(c).locator('.prj-orb-stop').click(),
    ConfirmStop: (c) => p(c).getByRole('dialog').getByRole('button', { name: 'Stop orb' }).click(),
    CancelStop: (c) => p(c).keyboard.press('Escape'),
    async OrbUp(c) { c.orb = 'running'; await poll(c); },
    async OrbExit(c) { c.orb = 'stopped'; await poll(c); },

    async StartThread(c) {
      const name = queueTurn(c);
      await p(c).getByRole('button', { name: 'Start thread' }).click();
      await p(c).getByPlaceholder('What should the thread do?').fill(`task ${name}`);
      const res = p(c).waitForResponse((r) => new URL(r.url()).pathname === '/api/sessions' && r.request().method() === 'POST');
      await p(c).getByRole('dialog').getByRole('button', { name: 'Start', exact: true }).click();
      const created = await res;
      if (!created.ok()) throw new Error(`start thread: ${created.status()} ${await created.text()}`);
      c.thread = ((await created.json()) as { session: { id: string } }).session.id;
      await threadRunning(c, name);
    },
    async ThreadFinish(c) {
      release(c.dir, c.turn);
      c.turn = '';
      await until(`thread ${c.thread}'s turn to end`, () => !turnOpen(entries(c, c.thread)));
      // The finish wakes main with a report; it answers at once.
      await mainQuiet(c, ++c.mainIn);
      // The App's list brings the finish (and acks it when it is on screen), then the page's poll.
      await poll(c, LIST_MS);
      await poll(c);
    },
    async ThreadMessage(c) {
      const name = queueTurn(c);
      await p(c).locator('#composer').fill(`turn ${name}`);
      await p(c).locator('#composer').press('Enter');
      await threadRunning(c, name);
      await poll(c);
    },
    async DropOnDone(c) {
      const row = threadRow(c);
      const box = (await row.boundingBox())!;
      await p(c).mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await p(c).mouse.down();
      await p(c).mouse.move(box.x + box.width / 2, box.y + box.height / 2 + 8, { steps: 4 });
      // Done is drawn as a target once the thread is in the air.
      const target = p(c).locator('.prj-group[data-drop]').filter({ hasText: 'Done' });
      await target.waitFor();
      const t = (await target.boundingBox())!;
      await p(c).mouse.move(t.x + t.width / 2, t.y + t.height / 2, { steps: 6 });
      await p(c).mouse.up();
      await poll(c);
    },
    // Archived by another client (the sidebar, another tab): this page has no control for it.
    async ArchiveThread(c) {
      const res = await c.serve.api.post(`/api/sessions/${c.thread}/archive`, { data: {} });
      if (!res.ok()) throw new Error(`archive thread: ${res.status()} ${await res.text()}`);
      c.thread = '';
      await poll(c);
    },
  },

  read: readUiState,
  status: (c) => c.page.locator('.prj-loading .pending, .prj-loading .error-note, .prj-bar h1.prj-name').first(),
  sessions: () => [],
  invariants: async (c, where) => {
    // Judged at rest: a hover or a colour fading in (the CSS runs on real
    // time, not the walk's clock) reads as low contrast half way.
    await Promise.race([
      c.page.evaluate(() => Promise.all(document.getAnimations()
        .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
        .map((a) => a.finished.then(() => {}, () => {})))),
      sleep(2_000),
    ]);
    await uiInvariants(c.page, {
    include: ['.prj', '.prj-loading', '[role=dialog]'],
    // Beside a conversation a thread's title ellipsizes by design, in the
    // crumb (a narrow column used to clip the way back instead) and in
    // the conversation's header alike, each with the whole title in its
    // tooltip; the home's counts line is held to it.
    text: '.prj-name, .prj-crumb, .prj-crumb:not(:has(.prj-back)) .prj-crumb-rest, .prj-group-label, .prj-orb-line, .prj-first-line, .prj-first-err, .prj-drawer-head h2, .file-said, .orb-save-err, .pending-msg, .error-note-title',
  }, where); },
  async cleanup(c) {
    if (c.turn) release(c.dir, c.turn);
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
