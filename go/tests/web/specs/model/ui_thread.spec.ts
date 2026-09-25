// go/tests/model/specs/ui_thread.fizz walked in the browser (recipe:
// go/tests/model/README.md): one open thread as the person sees it, its
// header status, the Settings popover, the Changes chip and the phone's
// Details, the running Working fold and its call row, the reply preview,
// the error card's Retry, jump-to-latest, and the sub pages. Every
// generated path is one test against its own serve (llm-control is the
// model); at every node readUiState must equal the spec's Thread#0 state.
//
// The walk decides when the server's steps happen:
//
//   - the first transcript read (GET /api/sessions/<id>, no cursor) is
//     held until Loaded lets it through or LoadFails answers it with a
//     body that is not JSON (a 5xx would do the same to the page, but
//     Chromium logs it as a console error);
//   - POST /prompt is held from Send (or the error card's Retry) until
//     Take lets it through, so "Sending…" is a state the page sits in;
//   - the turn is an llm-control "block" turn: Delta streams a fragment,
//     CallStart releases it into one bash call that waits on a gate file
//     (CallOk opens it, CallFails opens it with a fail marker), and the
//     next request is held again until Finish or Fail.
//
// The session runs in a git checkout, so a call that writes a file is
// the turn's edit: the footer's "N files" link, the phone's chip link.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

// Tall enough that a retried turn's card and prompt leave the failed
// turn's call row on screen above them.
const WIDE = { width: 1100, height: 800 };
const PHONE = { width: 600, height: 700 };

// Up to ~40 nodes a path, a few of them real turns.
test.describe.configure({ timeout: 180_000 });
// A step that cannot find its control fails there, not at the walk's timeout.
test.use({ actionTimeout: 10_000 });

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;          // llm-control's queue
  n: number;            // turn and gate names are unique across the walk
  firstRead: boolean;   // hold the transcript's first read
  load: Route | null;   // the first read, held
  post: Route | null;   // POST /prompt, held
  held: string;         // the model request the turn holds, '' when none
  next: string;         // queued for the request after the call
  gate: string;         // the running call's gate file
  said: string;         // the fragment streamed and not yet recorded
  thread: Record<string, unknown>; // the last state read on the thread itself
}

const name = (c: Ctx, p: string) => `${p}${String(++c.n).padStart(4, '0')}`;

function git(dir: string, home: string, ...args: string[]): void {
  execFileSync('git', ['-C', dir, '-c', 'user.name=model', '-c', 'user.email=model@test', '-c', 'commit.gpgsign=false', ...args], {
    env: { ...process.env, HOME: home, GIT_CONFIG_NOSYSTEM: '1' },
    stdio: 'pipe',
  });
}

async function until<T>(what: string, fn: () => Promise<T | undefined> | T | undefined, ms = 15_000): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v !== undefined && v !== false) return v as T;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

async function apiStatus(c: Ctx): Promise<string> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  if (!res.ok()) throw new Error(`session: ${res.status()}`);
  return (await res.json()).session.status;
}

function put(file: string, body: string): void {
  fs.writeFileSync(file + '-tmp', body);
  fs.renameSync(file + '-tmp', file);
}

// A fragment streamed while the turn is held: on screen, not recorded.
async function stream(c: Ctx, text: string): Promise<void> {
  const dst = path.join(c.dir, c.held + '.stream');
  put(dst, text);
  await until(`${c.held} to stream`, () => !fs.existsSync(dst) || undefined, 10_000);
}

// The page's own words, read off the DOM only.
async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await c.page.evaluate((id) => {
    const q = (sel: string) => document.querySelector(sel);
    const hash = location.hash;
    const route = hash.startsWith(`#/s/${id}/changes`) ? 'changes' : hash.startsWith(`#/s/${id}/context`) ? 'context' : hash === `#/s/${id}` ? 'thread' : `other: ${hash}`;
    const thread = q('.thread');
    const failed = [...document.querySelectorAll('.transcript-state')].some((e) => /Couldn’t load this session/.test(e.textContent ?? ''));
    // Read: the header names a status (it names none until then).
    const headStatus = q('.thread-head .head-main > .status');
    const transcript = failed ? 'failed' : headStatus ? 'ok' : 'loading';
    // The sidebar row carries the server's status in its label: "<title>,
    // <word>, …"; a failure carries its reason and the red second line.
    const row = q(`button.row[data-id="${id}"]`);
    const label = row?.getAttribute('aria-label') ?? '';
    const bad = !!row?.querySelector('.row-meta-bad');
    // The latest turn's call row (a send still on its way is no turn yet):
    // a retried turn has none of its own.
    const turns = [...(thread?.querySelectorAll('section.turn:not(.turn-sending)') ?? [])];
    const calls = [...(turns[turns.length - 1]?.querySelectorAll<HTMLDetailsElement>('details.call-native') ?? [])];
    const last = calls[calls.length - 1];
    const call = !last ? 'none' : last.querySelector('.tool-running') ? 'running' : last.classList.contains('block-failed') ? 'failed' : 'ok';
    const jump = q('button.jump-latest[aria-label$="jump to latest"], button.jump-latest[aria-label="Jump to latest"]');
    const strip = q('.thread-head .runtime-strip');
    // The chip itself, not the Details overflow (also a details.rt-jobs),
    // which holds it for a frame after a phone widens, before the strip unfolds.
    const chg = [...(strip?.querySelectorAll<HTMLDetailsElement>('details.rt-jobs:not(.rt-more)') ?? [])].find((d) => d.querySelector('a.chg-full'));
    const more = strip?.querySelector<HTMLDetailsElement>('details.rt-more');
    const settings = q('.head-pop[role="dialog"]');
    const a = document.activeElement;
    const focus = settings && settings.contains(a) ? 'settings'
      : a && (a.matches('.runtime-strip summary') || a.matches('.thread-head button.more')) ? 'trigger' : 'other';
    return {
      route,
      transcript,
      label, bad,
      sending: !!q('.turn-sending-state'),
      streamed: [...document.querySelectorAll('.stream-say')].some((e) => (e.textContent ?? '').trim() !== ''),
      call,
      callOpen: !!last?.open,
      workOpen: !!q('details.work-seg.work-seg-live[open]'),
      away: !!jump,
      fresh: (jump?.getAttribute('aria-label') ?? '').startsWith('New activity'),
      settings: !!settings,
      chg: !!chg?.open,
      details: !!more?.open,
      focus,
      usage: !!strip?.querySelector('[aria-label^="Context: "]'),
      edits: !!thread?.querySelector('.turn-files'),
      errCard: !!thread?.querySelector('.err-card'),
      wide: window.innerWidth > 720,
    };
  }, c.id);

  const word = dom.label.split(', ')[1] ?? '';
  const WORDS: Record<string, string> = { Idle: 'idle', Running: 'running', Done: 'done' };
  const status = WORDS[word] ?? (dom.bad ? 'error' : `unknown: ${dom.label}`);
  const shown: Record<string, unknown> = {
    viewport: dom.wide ? 'wide' : 'phone',
    route: dom.route,
    transcript: dom.transcript,
    status,
    sending: dom.sending,
    streamed: dom.streamed,
    call: dom.call,
    callOpen: dom.callOpen,
    workOpen: dom.workOpen,
    away: dom.away,
    fresh: dom.fresh,
    settings: dom.settings,
    chg: dom.chg,
    details: dom.details,
    focus: dom.focus,
    usage: dom.usage,
    edits: dom.edits,
    errCard: dom.errCard,
  };
  // A sub page replaces the Thread: what the thread showed when it was
  // left is the adapter's to remember (nothing moves it while the sub
  // page is up; Back reads the thread again).
  if (dom.route === 'thread') {
    c.thread = shown;
    return shown;
  }
  const kept = ['transcript', 'sending', 'streamed', 'call', 'callOpen', 'workOpen', 'away', 'fresh', 'usage', 'edits', 'errCard'];
  return { ...shown, ...Object.fromEntries(kept.map((k) => [k, c.thread[k]])) };
}

// A click where the row is on screen. Playwright's click scrolls its
// target into view first, and on a row at the very end of the
// transcript that scroll (centring it) is what took the reader off the
// end, not the page; a person clicks the row where it stands. The row
// is waited for until it stops moving (a jump's smooth scroll).
// A row a fold just opened below the view is scrolled to with the wheel,
// as a reader reaches it.
async function clickInPlace(c: Ctx, target: ReturnType<Page['locator']>): Promise<void> {
  await target.waitFor({ state: 'visible', timeout: 10_000 });
  const still = async () => {
    let last = '';
    return until('the row to stand still', async () => {
      const b = await target.boundingBox();
      const now = JSON.stringify(b);
      const ok = b && now === last;
      last = now;
      if (!ok) await new Promise((r) => setTimeout(r, 50));
      return ok ? b : undefined;
    }, 10_000);
  };
  const view = (await c.page.locator('.thread .transcript').boundingBox())!;
  let box = await still();
  const bottom = view.y + view.height;
  if (box.y + 24 > bottom) {
    await c.page.mouse.move(view.x + view.width / 2, view.y + view.height / 2);
    await c.page.mouse.wheel(0, box.y + 24 - bottom + 16);
    box = await still();
  }
  const y = box.y + Math.min(box.height, 24) / 2;
  if (y < view.y || y > bottom) throw new Error(`the row is not on screen (row at ${y}, transcript ${view.y}..${bottom})`);
  await c.page.mouse.click(box.x + Math.min(box.width, 40) / 2, y);
}

// The send the page makes, held until Take.
async function heldPost(c: Ctx): Promise<void> {
  await until('POST /prompt', () => c.post !== null || undefined);
}

// Long enough that the transcript is taller than the pane once shown in
// full (the bubble wraps it as one paragraph).
const PROMPT = 'Tidy the thread\n' + Array.from({ length: 800 }, (_, i) => `word${i + 1}`).join(' ');

modelTests<Ctx>({
  spec: 'ui_thread',
  role: 'Thread#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    // A checkout, so the call's write is the session's edit.
    git(serve.work, serve.home, 'init', '-q', '-b', 'main');
    fs.writeFileSync(path.join(serve.work, 'base.txt'), 'base\n');
    git(serve.work, serve.home, 'add', 'base.txt');
    git(serve.work, serve.home, 'commit', '-q', '-m', 'base');
    await page.setViewportSize(WIDE);
    const c: Ctx = {
      page, serve, id: await serve.newSession(), dir: controlDir(serve.home), n: 0,
      firstRead: true, load: null, post: null, held: '', next: '', gate: '', said: '',
      thread: {},
    };
    fs.mkdirSync(c.dir, { recursive: true });
    await page.route((u) => u.pathname === `/api/sessions/${c.id}`, (r) => {
      if (r.request().method() !== 'GET' || !c.firstRead || new URL(r.request().url()).searchParams.has('since')) return r.continue();
      c.load = r;
    });
    await page.route((u) => u.pathname === `/api/sessions/${c.id}/prompt`, (r) => { c.post = r; });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await until('the transcript read', () => c.load !== null || undefined);
    return c;
  },

  actions: {
    // --- the transcript read
    async Loaded(c) {
      c.firstRead = false;
      const r = c.load!;
      c.load = null;
      await r.continue();
      await c.page.locator('.thread-head .head-main > .status').waitFor({ timeout: 10_000 });
    },
    async LoadFails(c) {
      c.firstRead = false;
      const r = c.load!;
      c.load = null;
      await r.fulfill({ status: 200, contentType: 'application/json', body: 'transcript unavailable' });
    },
    async RetryLoad(c) {
      c.firstRead = true;
      await c.page.locator('.transcript-state').getByRole('button', { name: 'Retry' }).click();
      await until('the transcript read', () => c.load !== null || undefined);
    },

    // --- the person
    async Send(c) {
      await c.page.locator('#composer').fill(PROMPT);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      await heldPost(c);
    },
    async RetryTurn(c) {
      const cards = c.page.locator('.thread .err-card');
      await cards.last().getByRole('button', { name: 'Retry' }).click();
      await heldPost(c);
    },
    ToggleCall: (c) => clickInPlace(c, c.page.locator('.thread section.turn:not(.turn-sending)').last().locator('details.call-native').last().locator(':scope > summary')),
    ToggleWork: (c) => clickInPlace(c, c.page.locator('.thread details.work-seg.work-seg-live > summary')),
    // Scroll the transcript up by the wheel. The prompt is clamped to
    // four lines; shown in full it is taller than the pane.
    async ScrollUp(c) {
      // Expanding it leaves the scroll where it was, at the top of what
      // used to fit: down to the end first, as a reader would, then up.
      const toggle = c.page.locator('.thread .prompt-bubble').getByRole('button', { name: /^(Show full prompt|Show less)$/ }).first();
      await toggle.waitFor({ timeout: 10_000 });
      if ((await toggle.textContent()) === 'Show full prompt') await toggle.click();
      const t = c.page.locator('.thread .transcript');
      await t.hover();
      await c.page.mouse.wheel(0, 20_000);
      await until('the end of the transcript', () => t.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight < 40 || undefined), 5_000);
      await c.page.mouse.wheel(0, -600);
      await c.page.locator('button.jump-latest').filter({ hasText: /Latest|New activity/ }).waitFor({ timeout: 10_000 });
    },
    async JumpLatest(c) {
      await c.page.locator('button.jump-latest').filter({ hasText: /Latest|New activity/ }).click();
    },
    async OpenSettings(c) {
      await c.page.getByRole('button', { name: 'Session settings' }).click();
      await c.page.locator('.head-pop[role="dialog"]').waitFor({ timeout: 10_000 });
    },
    async OpenChanges(c) {
      await c.page.locator('.runtime-strip .rt-metrics > details.rt-jobs:not(.rt-more):has(a.chg-full) > summary').click();
    },
    async OpenDetails(c) {
      await c.page.locator('.runtime-strip details.rt-more > summary').click();
    },
    async Escape(c) {
      await c.page.keyboard.press('Escape');
    },
    // Mousedown on the transcript's empty space.
    async ClickAway(c) {
      const box = await c.page.locator('.thread .transcript').boundingBox();
      await c.page.mouse.click(box!.x + box!.width - 8, box!.y + box!.height - 8);
    },
    async OpenFullChanges(c) {
      const chg = c.page.locator('.runtime-strip details.rt-jobs:not(.rt-more)[open] a.chg-full');
      const phone = c.page.locator('.runtime-strip details.rt-more[open] a.rt-link[href$="/changes"]');
      if (await chg.count()) await chg.click();
      else if (await phone.count()) await phone.click();
      else await c.page.locator('.thread .turn-files').first().click();
      await until('the Changes page', async () => (await c.page.evaluate(() => location.hash)).includes('/changes') || undefined);
    },
    async OpenContext(c) {
      await c.page.locator('.runtime-strip [aria-label^="Context: "]').first().click();
      await until('the Context page', async () => (await c.page.evaluate(() => location.hash)).includes('/context') || undefined);
    },
    async Back(c) {
      await c.page.keyboard.press('Escape');
      await until('the thread', async () => (await c.page.evaluate(() => location.hash)) === `#/s/${c.id}` || undefined);
    },
    async Resize(c) {
      const wide = await c.page.evaluate(() => window.innerWidth > 720);
      await c.page.setViewportSize(wide ? PHONE : WIDE);
    },

    // --- the server
    async Take(c) {
      c.held = name(c, 't');
      queue(c.dir, c.held, { mode: 'block' });
      const r = c.post!;
      c.post = null;
      const answered = c.page.waitForResponse((res) => res.url().endsWith(`/api/sessions/${c.id}/prompt`));
      await r.continue();
      const res = await answered;
      if (!res.ok()) throw new Error(`prompt: ${res.status()}`);
      await waitTaken(c.dir, c.held);
      await until('running', async () => (await apiStatus(c)) === 'running' || undefined);
    },
    async Delta(c) {
      c.said = `${name(c, 'f')} streamed`;
      await stream(c, c.said);
    },
    // The reply so far is recorded with the call: the preview goes.
    async CallStart(c) {
      c.next = name(c, 't');
      c.gate = path.join(c.serve.home, `${c.next}.gate`);
      queue(c.dir, c.next, { mode: 'block' });
      const out = path.join(c.serve.work, 'edited.txt');
      const cmd = `while [ ! -e ${c.gate} ]; do sleep 0.05; done; if [ -e ${c.gate}.fail ]; then exit 1; fi; echo ${c.next} >> ${out}`;
      put(path.join(c.dir, c.held + '.release'), JSON.stringify({ text: c.said, call: { name: 'bash', args: { command: cmd } } }));
      c.said = '';
      c.held = '';
    },
    async CallOk(c) {
      fs.writeFileSync(c.gate, '');
      c.gate = '';
      await waitTaken(c.dir, c.next);
      c.held = c.next;
      c.next = '';
    },
    async CallFails(c) {
      fs.writeFileSync(c.gate + '.fail', '');
      fs.writeFileSync(c.gate, '');
      c.gate = '';
      await waitTaken(c.dir, c.next);
      c.held = c.next;
      c.next = '';
    },
    async Finish(c) {
      releaseWith(c.dir, c.held, { mode: 'ok', text: `finished ${c.held}` });
      c.held = '';
      c.said = '';
      await until('done', async () => (await apiStatus(c)) !== 'running' || undefined);
    },
    async Fail(c) {
      releaseWith(c.dir, c.held, { mode: 'error', error: 'model says no' });
      c.held = '';
      c.said = '';
      await until('the error', async () => (await apiStatus(c)) !== 'running' || undefined);
    },
  },

  read: readUiState,
  // The header's title: on screen on the thread; a sub page has its own head.
  status: (c) => c.page.locator('.thread-head h1, .page-head h1, main h1').first(),
  // The spec is the page's state, not a transcript's: no history check.
  sessions: () => [],
  async cleanup(c) {
    if (!c) return;
    c.firstRead = false;
    if (c.load) await c.load.continue().catch(() => {});
    if (c.post) await c.post.abort().catch(() => {});
    if (c.gate) fs.writeFileSync(c.gate, '');
    if (c.held) release(c.dir, c.held);
    if (c.next) releaseWith(c.dir, c.next, { mode: 'ok', text: 'cleanup' });
  },
});
