// go/tests/model/specs/project_main_threads.fizz in the browser: the
// project page (#/projects/<slug>) with its main thread, one thread and
// the report the thread's finish leaves in main. The Go twin is
// go/tests/model/mbt/project_main_threads_test.go; the server is set up
// exactly as there (llm-control with hold_boot, the fake orb runtime),
// and each path gets a fresh project on the worker's one serve.
//
// The page's own requests are held in the browser (page.route) so the
// spec's steps that are over in milliseconds can be read between: the
// POST is held until EnterA (the spec's "wait"), and its answer until
// DoneA (or MainReady, for a 'New thread', which the spec fuses). serve
// holds main's boot itself (hold_boot) until MainReady.
//
// What the page shows is read off the DOM: which view it is on (and
// that the URL says the same), whether main is listed, the thread's
// group in the list, the archived line, the composer's Sending…. What
// has no surface on the page is read the way the Go adapter reads it and
// says so where it is read: main's boot and the count of mains (serve's
// meta.json, llm-control's boot dir), who a thread reports to, and the
// request bookkeeping of the page and of the other tab (a, b, lock).
import * as fs from 'fs';
import * as path from 'path';
import type { APIResponse, Page, Route } from '@playwright/test';
import { controlDir, queue, release } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const CONFIG = '- id: llm\n  plugin: llm-control\n  config:\n    hold_boot: true\n' +
  '- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n';

const WAIT_MS = 20_000;

/** One request of the page's (a) or the other tab's (b). */
interface Req {
  text: string;        // a message's text, '' for a 'New thread'
  started: boolean;    // sent on to serve
  start(): void;
  answer(): Promise<APIResponse>;
  /** Lets the page have the answer; resolves once it has acted on it. */
  finish(): Promise<void>;
  /** The page's project composer is showing it as Sending…. */
  composer: boolean;
}

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;
  slug: string;
  tag: string;          // this path's prefix for turn and message names
  a: string; b: string; lock: string; asked: boolean;
  pa?: Req; pb?: Req;
  hold?: (r: Route) => void; // armed by an action that sends a request the walk holds
  thread: string;       // the thread's id, '' for none
  held: string;         // the thread's turn in flight
  turn: number; msg: number;
  mains: string[]; kids: string[];
}

let pathSeq = 0;

// --- serve's files, for what the page has no surface for

interface Meta { sessions?: Record<string, { project?: string; spawnedBy?: string; thread?: boolean; archived?: boolean }>; mains?: Record<string, string> }

function meta(c: Ctx): Meta {
  try { return JSON.parse(fs.readFileSync(path.join(c.serve.home, '.bough', 'serve', 'meta.json'), 'utf8')) as Meta; }
  catch { return {}; }
}

const histPath = (c: Ctx, id: string) => path.join(c.serve.home, '.bough', 'history', id + '.jsonl');
const hasHistory = (c: Ctx, id: string) => fs.existsSync(histPath(c, id));
const bootDir = (c: Ctx) => path.join(c.dir, 'boot');

/** llm-control's hold_boot: sessions held before their history file, id to role. */
function booting(c: Ctx): Record<string, string> {
  const out: Record<string, string> = {};
  let names: string[] = [];
  try { names = fs.readdirSync(bootDir(c)); } catch { return out; }
  for (const n of names) {
    if (!n.endsWith('.waiting')) continue;
    const id = n.slice(0, -'.waiting'.length);
    if (fs.existsSync(path.join(bootDir(c), id + '.release'))) continue;
    out[id] = fs.readFileSync(path.join(bootDir(c), n), 'utf8');
  }
  return out;
}

function releaseBoot(c: Ctx, id: string): void {
  fs.mkdirSync(bootDir(c), { recursive: true });
  fs.writeFileSync(path.join(bootDir(c), id + '.release'), '');
}

/** The spec's main, as serve records it: none, starting (boot held), live (history), gone. */
function mainState(c: Ctx): { st: string; id: string } {
  const id = meta(c).mains?.[c.slug] ?? '';
  if (!id) return { st: 'none', id };
  if (hasHistory(c, id)) return { st: 'live', id };
  const role = booting(c)[id];
  if (role !== undefined) return { st: role === 'main' ? 'starting' : `starting as ${role}`, id };
  return { st: 'gone', id };
}

/** mains: the project's sessions with no parent whose history exists or whose boot is held. */
function mainCount(c: Ctx): number {
  const held = booting(c);
  return Object.entries(meta(c).sessions ?? {})
    .filter(([id, m]) => m.project === c.slug && !m.spawnedBy && (id in held || hasHistory(c, id))).length;
}

interface Entry { kind: string; data?: Record<string, unknown> }

function entries(c: Ctx, id: string): Entry[] {
  try {
    return fs.readFileSync(histPath(c, id), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l) as Entry);
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

/** Reports from thread in main's transcript: inputs main woke on, naming it. */
function notices(c: Ctx, main: string, thread: string): number {
  return entries(c, main).filter((e) => e.kind === 'input' && e.data?.reason === 'notice' && String(e.data?.text ?? '').includes(thread)).length;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until(what: string, f: () => boolean | Promise<boolean>): Promise<void> {
  const deadline = Date.now() + WAIT_MS;
  while (!(await f())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}`);
    await sleep(20);
  }
}

/** Main's turn on has closed: an open turn on main would take the next queued turn. */
const mainQuiet = (c: Ctx, main: string, has: (es: Entry[]) => boolean) =>
  until(`main ${main} to settle`, () => { const es = entries(c, main); return has(es) && !turnOpen(es); });

const hasInput = (text: string) => (es: Entry[]) => es.some((e) => e.kind === 'input' && e.data?.text === text);

/** A server that would wrongly let a second caller past Main's lock gets the time to show it. */
const settle = () => sleep(200);

const waitMainBoot = (c: Ctx) => until(`a main of ${c.slug} held at boot`, () => mainState(c).st === 'starting');

// --- requests

/** The page's own request, caught by page.route and held. */
function pageReq(route: Route, text: string, composer: boolean): Req {
  let answer: Promise<APIResponse> | undefined;
  return {
    text, composer,
    get started() { return answer !== undefined; },
    start() { answer ??= route.fetch({ timeout: 60_000 }); },
    answer() { this.start(); return answer!; },
    async finish() { await route.fulfill({ response: await this.answer() }); },
  };
}

/** A request from another client: the other tab, or the page's message sent where its composer is not. */
function apiReq(c: Ctx, url: string, text: string, then?: () => Promise<void>): Req {
  let answer: Promise<APIResponse> | undefined;
  return {
    text, composer: false,
    get started() { return answer !== undefined; },
    start() { answer ??= c.serve.api.post(url, { data: { text }, timeout: 60_000 }); },
    answer() { this.start(); return answer!; },
    async finish() { await this.answer(); await then?.(); },
  };
}

/** The server's answer to r, which must be a success; a message's turn on main closed. */
async function collect(c: Ctx, r: Req): Promise<Record<string, unknown>> {
  const res = await r.answer();
  if (!res.ok()) throw new Error(`request ${r.text || 'new thread'}: ${res.status()} ${await res.text()}`);
  const body = (await res.json()) as Record<string, unknown>;
  if (r.text) await mainQuiet(c, String(body.main ?? mainState(c).id), hasInput(r.text));
  return body;
}

/** Arms the route for the next request the page sends, clicks, and returns what it caught. */
async function caught(c: Ctx, click: () => Promise<void>): Promise<Route> {
  const got = new Promise<Route>((resolve) => { c.hold = resolve; });
  await click();
  const route = await Promise.race([got, sleep(WAIT_MS).then(() => { throw new Error('the page sent no request'); })]);
  c.hold = undefined;
  return route;
}

function threadOf(body: Record<string, unknown>): string {
  const id = String((body.session as { id?: string } | undefined)?.id ?? '');
  if (!id) throw new Error(`new thread: no session in ${JSON.stringify(body)}`);
  return id;
}

const nextMessage = (c: Ctx) => `${c.tag} message ${++c.msg}`;

function queueTurn(c: Ctx): string {
  const name = `${c.tag}-p${String(++c.turn).padStart(4, '0')}`;
  queue(c.dir, name, { mode: 'block', text: `finished ${name}` });
  return name;
}

async function running(c: Ctx, name: string): Promise<void> {
  await until(`turn ${name} taken`, () => fs.existsSync(path.join(c.dir, name + '.taken')));
  c.held = name;
  await until(`thread ${c.thread} in a turn`, () => turnOpen(entries(c, c.thread)));
}

// --- the page

const p = (c: Ctx) => c.page;
const onHome = (c: Ctx) => p(c).locator('.prj-home').isVisible();
const threadRows = (c: Ctx) => p(c).locator(`.prj-thread:not(.prj-main-row)`);

/** The row of the thread in whichever list is on screen, unfolding its group first as a person would. */
async function threadRow(c: Ctx) {
  const shown = p(c).locator('.prj-threads-list, .prj-queue').first();
  const more = shown.locator('.prj-more');
  if (await more.count()) await more.first().click();
  for (const head of await shown.locator('button.prj-group-head[aria-expanded="false"]').all()) await head.click();
  return threadRows(c).first();
}

/** The groups of the list on screen: label and count. */
async function groups(c: Ctx): Promise<{ label: string; n: number }[]> {
  const list = p(c).locator('.prj-threads-list, .prj-queue').first();
  if (!(await list.count())) return [];
  return list.locator('.prj-group').evaluateAll((els) => els.map((g) => ({
    label: g.querySelector('.prj-group-label')?.textContent ?? '',
    n: Number(g.querySelector('.prj-group-count')?.textContent ?? 0),
  })));
}

/** A locator's text, '' when it is not there. Never waits: textContent
 *  on a node a re-render just removed waited out the whole test. */
const text = (l: ReturnType<Page['locator']>): Promise<string> => l.evaluateAll((els) => els[0]?.textContent ?? '');

const FINISHED = new Set(['Finished — unseen', 'Done', 'Idle']);

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const page = p(c);
  const m = mainState(c);
  const home = await onHome(c);
  const back = await page.getByRole('button', { name: '‹ All threads' }).isVisible();
  const crumb = back ? await text(page.locator('.prj-crumb-rest')) : '';

  // The view the page shows, and the URL must name the same thing: a
  // reload or a copied link reopens what the hash says.
  let view = home && !back ? 'home' : back ? (crumb === '· Main thread' ? 'main' : 'thread') : 'neither';
  const hash = await page.evaluate(() => window.location.hash);
  const want = view === 'home' ? `#/projects/${c.slug}` : view === 'main' ? `#/projects/${c.slug}/t/${m.id}` : `#/projects/${c.slug}/t/${c.thread}`;
  if (hash !== want) view = `${view}, but the URL says ${hash}`;

  // Main is live when the page lists it; the steps before that (its boot
  // held, or its history gone) look alike on the page and are read off serve.
  const listed = (await page.locator(home ? '.prj-main-row' : '.prj-main-thread').count()) > 0;
  const main = listed ? (m.st === 'live' ? 'live' : `listed while ${m.st}`) : m.st === 'live' ? 'live, not listed' : m.st;

  const archived = home
    ? (await text(page.locator('.prj-first-line'))) === 'Archived. A message reopens it.'
    : view === 'main' ? await page.locator('.archived-note').isVisible() : false;

  // The thread by its group in the list on screen; one thread, so at most one row.
  const gs = (await groups(c)).filter((g) => g.n > 0);
  let thread = 'none';
  if (gs.length > 1 || gs.some((g) => g.n > 1)) thread = `many: ${JSON.stringify(gs)}`;
  else if (gs.length === 1) {
    const g = gs[0].label;
    if (g === 'Empty') thread = 'empty';
    else if (FINISHED.has(g)) thread = 'reported';
    else if (g === 'Running') {
      // A booting thread is listed as running too (serve's pending row);
      // what tells them apart is that it has no transcript to show yet.
      thread = c.thread && !hasHistory(c, c.thread) ? 'starting' : 'running';
      if (view === 'thread' && thread === 'starting' && !(await page.getByText('Starting the session…').isVisible())) thread = 'starting, not said';
    } else thread = `group ${g}`;
  }

  // Nothing on the page says whom a thread reports to or whether it is a
  // full agent: serve's own record of the listed thread.
  let parented = false, full = false;
  if (thread !== 'none' && c.thread) {
    const sm = meta(c).sessions?.[c.thread];
    parented = Boolean(sm?.spawnedBy) && sm?.spawnedBy === m.id;
    full = Boolean(sm?.thread);
  }

  // a is the page's request; a message from the home composer says Sending… while it is out.
  const sending = await page.getByRole('button', { name: 'Sending…' }).isVisible();
  let a = c.a;
  if (c.pa?.composer && c.a.startsWith('msg') && !sending) a = `${c.a}, composer not sending`;
  if (c.a === 'idle' && sending) a = 'idle, composer sending';

  return { main, mains: mainCount(c), archived, lock: c.lock, a, b: c.b, asked: c.asked, view, thread, parented, full };
}

// --- actions

async function openMain(c: Ctx): Promise<void> {
  const row = (await onHome(c)) ? p(c).locator('.prj-main-row') : p(c).locator('.prj-main-thread');
  await row.click();
}

modelTests<Ctx>({
  spec: 'project_main_threads',
  role: 'Project#0',
  config: CONFIG,
  shared: true,

  async init(page, serve) {
    const tag = `w${process.pid}x${++pathSeq}`;
    const res = await serve.api.post('/api/projects', { data: { name: `Walk ${tag}` } });
    if (!res.ok()) throw new Error(`new project: ${res.status()} ${await res.text()}`);
    const slug = ((await res.json()) as { project: { slug: string } }).project.slug;
    const c: Ctx = {
      page, serve, dir: controlDir(serve.home), slug, tag,
      a: 'idle', b: 'idle', lock: '', asked: false,
      thread: '', held: '', turn: 0, msg: 0, mains: [], kids: [],
    };
    const held = async (route: Route) => {
      if (route.request().method() !== 'POST' || !c.hold) return route.fallback();
      c.hold(route);
    };
    await page.route((u) => u.pathname === `/api/projects/${slug}/message` || u.pathname === '/api/sessions', held);
    await page.goto(`${serve.url}/#/projects/${slug}`);
    await page.locator('h1.prj-name').waitFor();
    return c;
  },

  actions: {
    // The page reads the project every 4 s; nothing it reads starts main.
    Poll: (c) => p(c).clock.fastForward(4_500),

    // The home's composer; elsewhere on the page there is none (main's
    // own composer talks to main, not through Main(slug)), so the same
    // POST goes from the test and the page is sent to main at DoneA, as
    // the composer's onMessage does.
    async Message(c) {
      c.a = 'msg-wait'; c.asked = true;
      const text = nextMessage(c);
      if (await onHome(c)) {
        await p(c).getByRole('textbox', { name: 'Message the project' }).fill(text);
        const route = await caught(c, () => p(c).locator('.prj-first').getByRole('button', { name: 'Send', exact: true }).click());
        c.pa = pageReq(route, text, true);
      } else {
        c.pa = apiReq(c, `/api/projects/${c.slug}/message`, text, () => openMain(c));
      }
      if (c.lock === 'b') {
        // The other tab is inside Main: this one queues on the lock.
        c.pa.start();
        await settle();
      }
    },

    async NewThread(c) {
      c.a = 'thread-wait'; c.asked = true;
      const button = p(c).locator('.prj-queue-head, .prj-threads-head').first().getByRole('button', { name: 'New thread' });
      const route = await caught(c, () => button.click());
      c.pa = pageReq(route, '', false);
    },

    // The other tab: its request is sent at EnterB, as the Go adapter's.
    async MessageB(c) {
      c.b = 'wait'; c.asked = true;
      c.pb = apiReq(c, `/api/projects/${c.slug}/message`, nextMessage(c));
    },

    async EnterA(c) {
      const live = mainState(c).st === 'live';
      c.lock = 'a';
      c.a = c.a === 'msg-wait' ? 'msg-held' : 'thread-held';
      // Main() returns at once (or already did, queued behind b): the rest is DoneA's.
      if (live || c.pa!.started) return;
      c.pa!.start();
      await waitMainBoot(c);
      if (c.b === 'wait' && !c.pb!.started) c.pb!.start();
      await settle();
    },

    async EnterB(c) {
      const live = mainState(c).st === 'live';
      c.lock = 'b'; c.b = 'held';
      if (live) return;
      c.pb!.start();
      await waitMainBoot(c);
      if (c.a === 'msg-wait' && !c.pa!.started) c.pa!.start();
      await settle();
    },

    async MainReady(c) {
      const { id } = mainState(c);
      releaseBoot(c, id);
      await until(`main ${id}'s history`, () => hasHistory(c, id));
      c.mains.push(id);
      // Every request queued on the lock goes through now.
      for (const r of [c.pa, c.pb]) {
        if (!r?.started) continue;
        const body = await collect(c, r);
        if (r === c.pa && !r.text) {
          // A 'New thread' is created in the same handler: the page opens it (the spec's fused DoneA).
          c.thread = threadOf(body);
          c.kids.push(c.thread);
          await r.finish();
          c.pa = undefined;
          c.lock = ''; c.a = 'idle';
        }
      }
    },

    async DoneA(c) {
      const r = c.pa!;
      const body = await collect(c, r);
      await r.finish();
      c.pa = undefined;
      c.lock = '';
      if (!r.text) {
        c.thread = threadOf(body);
        c.kids.push(c.thread);
      }
      c.a = 'idle';
    },

    async DoneB(c) {
      await collect(c, c.pb!);
      c.pb = undefined;
      c.lock = ''; c.b = 'idle';
    },

    // Main's composer: Start thread asks for the task, and the thread boots straight into it.
    async StartThread(c) {
      const name = queueTurn(c);
      await p(c).getByRole('button', { name: 'Start thread' }).click();
      await p(c).getByPlaceholder('What should the thread do?').fill(`task ${name}`);
      const res = p(c).waitForResponse((r) => new URL(r.url()).pathname === '/api/sessions' && r.request().method() === 'POST');
      await p(c).getByRole('dialog').getByRole('button', { name: 'Start', exact: true }).click();
      const created = await res;
      if (!created.ok()) throw new Error(`start thread: ${created.status()} ${await created.text()}`);
      c.thread = threadOf((await created.json()) as Record<string, unknown>);
      c.kids.push(c.thread);
      releaseBoot(c, c.thread);
      await running(c, name);
    },

    async ThreadBoot(c) {
      releaseBoot(c, c.thread);
      await until(`thread ${c.thread}'s history`, () => hasHistory(c, c.thread));
      // launch writes a prompt, when there is one, right after the file appears.
      await settle();
    },

    // The open thread's composer.
    async ThreadMessage(c) {
      const name = queueTurn(c);
      await p(c).locator('#composer').fill(`turn ${name}`);
      await p(c).getByRole('button', { name: 'Send', exact: true }).click();
      await running(c, name);
    },

    async ThreadFinish(c) {
      release(c.dir, c.held);
      c.held = '';
      await until(`thread ${c.thread}'s turn to end`, () => !turnOpen(entries(c, c.thread)));
      const closed = entries(c, c.thread).filter((e) => e.kind === 'done').length;
      const main = mainState(c).id;
      const thread = c.thread;
      await mainQuiet(c, main, () => notices(c, main, thread) >= closed);
    },

    // The open thread is archived from its own settings; one not on
    // screen has no control on this page and is archived as another
    // client would (the sidebar, another tab).
    async ArchiveThread(c) {
      const back = await p(c).getByRole('button', { name: '‹ All threads' }).isVisible();
      const shown = back && (await text(p(c).locator('.prj-crumb-rest'))) !== '· Main thread';
      if (shown) {
        await p(c).getByRole('button', { name: 'Session settings' }).click();
        await p(c).locator('.head-pop-item', { hasText: /^Archive…$/ }).click();
        const done = p(c).waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.thread}/archive`) && r.request().method() === 'POST');
        await p(c).locator('[role="dialog"][aria-modal="true"]').getByRole('button', { name: 'Archive', exact: true }).click();
        const res = await done;
        if (!res.ok()) throw new Error(`archive thread: ${res.status()} ${await res.text()}`);
      } else {
        const res = await c.serve.api.post(`/api/sessions/${c.thread}/archive`, { data: {} });
        if (!res.ok()) throw new Error(`archive thread: ${res.status()} ${await res.text()}`);
      }
      c.thread = '';
    },

    // The project list archives a project; this page has no control for it.
    async ArchiveProject(c) {
      const res = await c.serve.api.post(`/api/projects/${c.slug}/archive`, { data: {} });
      if (!res.ok()) throw new Error(`archive project: ${res.status()} ${await res.text()}`);
    },

    OpenMain: openMain,
    OpenThread: async (c) => (await threadRow(c)).click(),
    Back: (c) => p(c).getByRole('button', { name: '‹ All threads' }).click(),

    // A person cleaning ~/.bough/history; main's process runs on, writing to nothing.
    async LoseMainHistory(c) {
      fs.rmSync(histPath(c, mainState(c).id));
    },
  },

  // A thread 404s until its history file exists and a main whose file
  // is gone 404s for good: the page reads the first as "starting" and
  // the second as no main, and Chromium logs each as a failed load.
  expectedErrors: /status of 404 \(Not Found\)/,

  read: readUiState,
  // The project's name heads the page whatever it shows.
  status: (c) => c.page.locator('h1.prj-name'),
  sessions: (c) => [...c.mains, ...c.kids],

  // Everything the path left held goes, and the project stops, so the
  // next path on this serve takes only its own turns.
  async cleanup(c) {
    const mine = new Set(Object.entries(meta(c).sessions ?? {}).filter(([, m]) => m.project === c.slug).map(([id]) => id));
    for (const id of Object.keys(booting(c))) if (mine.has(id)) releaseBoot(c, id);
    if (c.held) release(c.dir, c.held);
    // A turn queued and never taken would be the next path's first.
    for (const f of fs.existsSync(c.dir) ? fs.readdirSync(c.dir) : []) if (f.startsWith(c.tag + '-') && f.endsWith('.json')) fs.rmSync(path.join(c.dir, f), { force: true });
    for (const r of [c.pa, c.pb]) {
      if (r) await r.finish().catch(() => undefined);
    }
    await c.serve.api.post(`/api/projects/${c.slug}/archive`, { data: {} }).catch(() => undefined);
  },
});
