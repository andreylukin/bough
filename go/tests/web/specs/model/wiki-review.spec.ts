// go/tests/model/specs/wiki_review.fizz in the browser: every generated
// path through the control room's wiki (index, a page, a cited entry,
// Review, Activity) walked against a real serve. The Go twin is
// go/tests/model/mbt/wiki_review_test.go; the world here is the same one
// page with one uncited (flagged) claim, one good citation and one
// dangling one.
//
// The ingest is an external actor. The page's POST /api/wiki/ingest is
// answered at the network the moment it is sent (the page sees what a
// working serve says), and the walk forwards that same request to the
// serve only at the spec's Start: the spec lets a spawned run reach the
// wiki lock at any later step, and a real `bough wiki run` gets there in
// milliseconds, before a Session the spec interleaves could make
// anything pending. The run is then the real one, its one model turn held
// by llm-control so the walk decides whether it lands or times out; it
// may never finish on its own. llm-control answers with text only, so on
// Land the walk makes the ingest agent's writes (the page, log.md).
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

// A path is up to fourteen steps, some of them real ingest runs.
test.describe.configure({ timeout: 120_000 });

const PAGE = 'topics/demo/alpha.md';
const MISSING = 'topics/demo/none.md';
const SEED = 'seed-session';   // the good citation's session
const GHOST = 'ghost-session'; // the dangling citation's: no such file
const LIVE = ['review', 'activity'];

// The one page. The lede carries the dangling citation, the first fact
// the good one, and the second fact is the flagged claim: uncited, so
// Mark as inference and Drop both clear it. Every Land rewrites the
// claim's line to a new version, which is what makes Review's copy stale.
const pageBody = (version: number) =>
  `# Alpha\n\nThe demo page, begun in \`${GHOST}#3\`.\n\n## Facts\n\n` +
  `- The good fact rests on \`${SEED}#2\`.\n` +
  `- The flagged fact, version ${version}, has no entry behind it.\n`;

interface Run { turn: string; pid: number; ids: string[] }

interface Ctx {
  page: Page;
  serve: Serve;
  wiki: string;        // HOME/.bough/wiki
  history: string;     // HOME/.bough/history
  servePid: number;    // the `wiki run`s serve spawns are its children
  spawn: string | null; // the page's ingest POST body, sent to serve at Start; null when none is out
  held: Run[];         // runs whose model turn is held
  version: number;
  nsess: number;
  turn: number;
}



function write(file: string, body: string, at?: Date): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
  if (at) fs.utimesSync(file, at, at);
}

// A finished session's transcript (input, assistant, done: seq 1..3),
// dated `at` so the wiki's pending scan sees it as quiet.
function writeSession(file: string, at: Date, prompt: string): void {
  const entries = [
    { kind: 'input', data: { text: prompt } },
    { kind: 'assistant', data: { text: 'it works like this' } },
    { kind: 'done', data: {} },
  ].map((e, i) => JSON.stringify({ seq: i + 1, at: at.toISOString(), ...e, ...(i ? { parent: i } : {}) }));
  write(file, entries.join('\n') + '\n', at);
}

// --- the serve's side, for the store fields a screen does not show live
// and for the waits of the external actor. Never for what the page owns.

interface Flag { kind: string; page: string; line: number; raw: string }
async function get<T>(c: Ctx, url: string): Promise<T> {
  const res = await c.serve.api.get(url);
  if (!res.ok()) throw new Error(`GET ${url}: ${res.status()} ${await res.text()}`);
  return (await res.json()) as T;
}
const claimFlags = async (c: Ctx) =>
  (await get<{ flags: Flag[] }>(c, '/api/wiki/review')).flags.filter((f) => f.kind !== 'problem');
const storePending = async (c: Ctx) => (await get<{ pending: unknown[] }>(c, '/api/wiki/review')).pending.length > 0;
const storeIngesting = async (c: Ctx) =>
  (await get<{ runs: { running: boolean }[] }>(c, '/api/wiki/activity')).runs.filter((r) => r.running).length;

async function waitFor(what: string, ok: () => Promise<boolean> | boolean, ms = 20_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}: timed out after ${ms}ms`);
    await new Promise((r) => setTimeout(r, 20));
  }
}

// serve's `bough wiki run` children.
function wikiRuns(c: Ctx): number[] {
  const out = execFileSync('ps', ['-A', '-o', 'pid=,ppid=,command='], { encoding: 'utf8' });
  return out.split('\n').map((l) => l.trim().split(/\s+/)).filter((f) => f.length >= 4 && f[1] === String(c.servePid)
    && f.slice(2).join(' ').includes(' wiki run')).map((f) => Number(f[0]));
}

// A reaped or zombie `wiki run` no longer counts.
function alive(pid: number): boolean {
  try {
    const out = execFileSync('ps', ['-o', 'stat=,command=', '-p', String(pid)], { encoding: 'utf8' });
    return !out.trim().startsWith('Z') && out.includes(' wiki run');
  } catch {
    return false;
  }
}

// Lets the oldest held run finish (landed) or fail (timed out), and waits
// until its `wiki run` has exited and the serve stops counting it.
async function end(c: Ctx, ok: boolean): Promise<void> {
  const r = c.held.shift()!;
  const dir = controlDir(c.serve.home);
  if (ok) release(dir, r.turn);
  else releaseWith(dir, r.turn, { mode: 'error', error: 'ingest timed out' });
  await waitFor('the wiki run to exit', () => r.pid === 0 || !alive(r.pid));
  const n = c.held.length;
  await waitFor(`the serve to count ${n} running`, async () => (await storeIngesting(c)) === n);
}

// --- the page ----------------------------------------------------------

const nav = (c: Ctx) => c.page.locator('nav.side-nav:not(.phone-nav) a[href="#/wiki"]');
const doc = (c: Ctx) => c.page.locator('.wk-doc');
const claimItem = (c: Ctx) => c.page.locator('.wk-item').filter({ has: c.page.locator('.wk-tag', { hasText: 'Uncited' }) });

// A hash as the screen it opens (a history entry's, for Back).
function screenOf(hash: string): string {
  if (hash === '#/wiki') return 'index';
  if (hash === '#/wiki/review') return 'review';
  if (hash === '#/wiki/activity') return 'activity';
  if (hash.startsWith('#/wiki/p/')) {
    if (hash.includes('~' + SEED + '~')) return 'source';
    if (hash.includes('~' + GHOST + '~')) return 'source_error';
    return hash.includes(MISSING) ? 'missing' : 'page';
  }
  return hash.startsWith('#/wiki') ? `unknown ${hash}` : 'list';
}

// The screen as drawn: the hash says where the app thinks it is, the DOM
// what it shows; a disagreement reads as neither.
async function screenShown(c: Ctx, hash: string): Promise<string> {
  const p = c.page;
  const at = screenOf(hash);
  const has = async (sel: string) => (await p.locator(sel).count()) > 0;
  const h1 = async (t: string) => (await p.locator('.thread-head h1', { hasText: t }).count()) > 0;
  let dom: string;
  if (await has('#wk-body')) dom = 'editing';
  else if (await has('section.wk-src')) {
    const title = (await p.locator('.wk-src-title').textContent()) ?? '';
    dom = title === 'Entry not found' ? 'source_error' : (await has('.wk-src .wk-ent-on')) ? 'source' : 'source loading';
  } else if (await has('text=This page doesn’t exist')) dom = 'missing';
  else if (await has('.wk-titleblock')) dom = 'page';
  else if (await h1('Review') && await has('text=Not compiled')) dom = 'review';
  else if (await h1('Activity') && await has('.wk-stats')) dom = 'activity';
  else if (await has('.wk-health')) dom = 'index';
  else dom = at === 'list' ? 'list' : 'loading';
  return dom === at || (dom === 'editing' && at === 'page') ? dom : `${dom} at ${hash}`;
}

// readUiState: what the person can see. Page-owned fields come from the
// DOM. The store's (flagged, pending, ingesting) come from the screen
// that shows them live (the index's health line, Activity's queue and
// runs, the page's claim chips, Review's list while it lists the claim);
// where the screen on view does not show them, from the serve, as the Go
// adapter reads them. spawned is the external actor's: a POST the walk
// holds. back is the browser's own history, read through the Navigation
// API; the spec keeps one entry, so under a screen Back returned to
// (anything but Review or Activity, which sit on the index) it is "".
async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const nav0 = await p.evaluate(() => {
    const n = (window as unknown as { navigation: { entries(): { url: string }[]; currentEntry: { index: number } } }).navigation;
    const es = n.entries(), i = n.currentEntry.index;
    const hash = (u: string) => new URL(u).hash;
    return { hash: location.hash, prev: i > 0 ? hash(es[i - 1].url) : null, forward: i < es.length - 1 };
  });
  const screen = await screenShown(c, nav0.hash);
  const back = nav0.prev === null || (nav0.forward && !LIVE.includes(screen)) ? '' : screenOf(nav0.prev);

  const listed = screen === 'review' && (await claimItem(c).count()) > 0;
  const conflict = screen === 'review' && (await p.getByText('Did not save', { exact: false }).count()) > 0;
  let moved = false;
  if (listed) {
    const shown = /version (\d+)/.exec((await claimItem(c).first().locator('.wk-item-claim').textContent()) ?? '')?.[1];
    const now = (await claimFlags(c)).map((f) => /version (\d+)/.exec(f.raw)?.[1]);
    moved = !now.includes(shown);
  }

  let flagged: boolean;
  if (screen === 'index') flagged = (await p.locator('.wk-health .wk-link-bad', { hasText: /^Review \d+ flagged claim/ }).count()) > 0;
  else if (['page', 'source', 'source_error'].includes(screen)) flagged = (await doc(c).locator('.wk-claim[data-state="uncited"]').count()) > 0;
  else if (listed) flagged = true;
  else flagged = (await claimFlags(c)).length > 0;

  let pending: boolean;
  let ingesting: number;
  if (screen === 'activity') {
    pending = (await p.locator('.wk-stats .hk2-sum-n.is-waiting').count()) > 0;
    ingesting = await p.locator('.wk-run-head .hk2-state', { hasText: /^Ingesting$/ }).count();
  } else if (screen === 'index') {
    pending = (await p.locator('.wk-health .wk-fact', { hasText: /waiting$/ }).count()) > 0;
    ingesting = (await p.locator('.wk-health .wk-fact', { hasText: 'Ingesting now' }).count()) > 0 ? 1 : 0;
  } else {
    pending = await storePending(c);
    ingesting = await storeIngesting(c);
  }

  const noteEl = p.locator('.thread-head .hk-note[role=status]');
  const noteText = screen === 'activity' && (await noteEl.count()) > 0 ? ((await noteEl.textContent()) ?? '') : '';
  const note = noteText === '' ? '' : noteText.startsWith('Started') ? 'started' : noteText.startsWith('Nothing') ? 'nothing' : `? ${noteText}`;

  return {
    screen, back, came: back, flagged, listed, moved, conflict, pending,
    spawned: c.spawn !== null, ingesting, note,
    badge: (await nav(c).locator('.nav-count').count()) > 0,
    // Ghosts: what the spec proves, not what a screen shows.
    injected: false, stalewrite: false,
  };
}

// The index's link when it offers one; with nothing flagged it does not,
// and the palette's "Review flagged claims" is the way there.
async function toReview(c: Ctx): Promise<void> {
  const link = c.page.locator('.wk-health .wk-link-bad');
  if (await link.count()) return link.click();
  await c.page.keyboard.press('ControlOrMeta+k');
  await c.page.keyboard.type('Review flagged claims');
  await c.page.getByRole('option', { name: /Review flagged claims/ }).first().click();
}

modelTests<Ctx>({
  spec: 'wiki_review',
  role: 'Wiki#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const pid = Number(fs.readFileSync(path.join(serve.home, '.bough', 'serve.pid'), 'utf8').trim().split(/\s+/)[0]);
    const c: Ctx = {
      page, serve, wiki: path.join(serve.home, '.bough', 'wiki'), history: path.join(serve.home, '.bough', 'history'),
      servePid: pid, spawn: null, held: [], version: 0, nsess: 0, turn: 0,
    };
    // The good citation's session, dated before the wiki's baseline so it
    // is never pending; the store as the spec's Init has it.
    writeSession(path.join(c.history, SEED + '.jsonl'), new Date(Date.now() - 72 * 3600_000), 'how the demo works');
    write(path.join(c.wiki, PAGE), pageBody(0));
    write(path.join(c.wiki, 'index.md'), `# Wiki index\n\n## demo\n\n- [Alpha](${PAGE}) — the demo page\n`);
    write(path.join(c.wiki, 'log.md'), `# Wiki log\n\n<!-- baseline: ${new Date(Date.now() - 48 * 3600_000).toISOString().replace(/\.\d+Z$/, 'Z')} -->\n`);

    // The nav count's 60 s re-read is the spec's Poll: the tab reads as
    // hidden, which the app's interval skips, except while Poll runs it.
    // Otherwise the harness's clock steps would run it at whatever step
    // crossed the minute.
    await page.addInitScript(() => {
      Object.defineProperty(Document.prototype, 'hidden', { configurable: true, get: () => !(window as unknown as { __visible?: boolean }).__visible });
    });
    await page.route((u) => u.pathname === '/api/wiki/ingest', async (route) => {
      if (route.request().method() !== 'POST') return route.fallback();
      if (c.spawn !== null) throw new Error('wiki_review: a second ingest POST while one is spawned');
      c.spawn = route.request().postData() ?? '';
      await route.fulfill({ status: 200, contentType: 'application/json', body: '{"ok":true}' });
    });
    await page.goto(`${serve.url}/#/`);
    return c;
  },

  actions: {
    Nav: (c) => nav(c).click(),
    async OpenPage(c) {
      if (c.page.url().includes('#/wiki/review')) await c.page.locator('.wk-where', { hasText: PAGE + ':' }).first().click();
      else await c.page.locator('a.wk-row', { hasText: 'Alpha' }).click();
    },
    // A typed (or stale) link: no screen links to a page that is not there.
    OpenMissing: (c) => c.page.evaluate((h) => { location.hash = h; }, `#/wiki/p/${MISSING}`),
    Cite: (c) => doc(c).locator('button.wk-cite:not(.wk-cite-bad)').first().click(),
    CiteDangling: (c) => doc(c).locator('button.wk-cite.wk-cite-bad').first().click(),
    CloseSource: (c) => c.page.getByRole('button', { name: 'Close the entry', exact: true }).click(),
    Edit: (c) => c.page.locator('.wk-page-acts').getByRole('button', { name: 'Edit', exact: true }).click(),
    async Save(c) {
      await c.page.getByRole('button', { name: 'Save', exact: true }).click();
      await c.page.locator('#wk-body').waitFor({ state: 'detached' });
    },
    ToIndex: (c) => c.page.locator('.thread-head .wk-crumb-link', { hasText: 'Wiki' }).first().click(),
    ToReview: toReview,
    ToActivity: (c) => c.page.locator('.thread-head').getByRole('button', { name: 'Activity', exact: true }).click(),
    Back: (c) => c.page.goBack().then(() => {}),
    MarkInference: (c) => claimItem(c).getByRole('button', { name: 'Mark as inference', exact: true }).click(),
    Drop: (c) => claimItem(c).getByRole('button', { name: 'Drop the claim', exact: true }).click(),
    async IngestNow(c) {
      await c.page.locator('.thread-head').getByRole('button', { name: 'Ingest now', exact: true }).click();
      await waitFor('the page to send Ingest now', () => c.spawn !== null, 5_000);
    },
    async IngestSession(c) {
      // The row's one button, whatever it says after an earlier click: a
      // run the lock turned away or that timed out leaves the session
      // pending, and the row is where it is started again.
      await c.page.locator('.proj-row').getByRole('button').first().click({ timeout: 5_000 });
      await waitFor('the page to send Ingest', () => c.spawn !== null, 5_000);
    },
    // A session finishes: a transcript an hour quiet, so it is pending.
    async Session(c) {
      const id = `pend-${String(++c.nsess).padStart(4, '0')}`;
      writeSession(path.join(c.history, id + '.jsonl'), new Date(Date.now() - 3600_000), `session ${id}`);
    },
    // The held POST reaches serve, and the `wiki run` it spawns gets past
    // the lock: its ingest takes the turn queued for it (a run), or it
    // exits (the lock was held, or nothing was pending).
    async Start(c) {
      const body = c.spawn ?? '';
      c.spawn = null;
      const only = (JSON.parse(body || '{}') as { session?: string }).session ?? '';
      const pending = (await get<{ pending: { id: string }[] }>(c, '/api/wiki/review')).pending.map((p) => p.id);
      const name = `ing${String(++c.turn).padStart(4, '0')}`;
      const dir = controlDir(c.serve.home);
      queue(dir, name, { mode: 'block', text: 'ingested' });
      const before = wikiRuns(c);
      const res = await c.serve.api.post('/api/wiki/ingest', { data: only ? { session: only } : {} });
      if (!res.ok()) throw new Error(`POST /api/wiki/ingest: ${res.status()} ${await res.text()}`);
      const pid = wikiRuns(c).find((p) => !before.includes(p)) ?? 0;
      const ids = pending.filter((id) => !only || id === only);
      const taken = path.join(dir, name + '.taken');
      let ran = false;
      await waitFor('the spawned wiki run to reach the lock', () => {
        if (fs.existsSync(taken)) return (ran = true);
        if (pid !== 0 && alive(pid)) return false;
        // It exited without asking the model: take the turn back, unless
        // it was taken between the two checks.
        try { fs.unlinkSync(path.join(dir, name + '.json')); return true; } catch { return false; }
      });
      if (ran) {
        c.held.push({ turn: name, pid, ids });
        const n = c.held.length;
        await waitFor(`the serve to count ${n} running`, async () => (await storeIngesting(c)) === n);
      }
    },
    // The ingest agent's work while its turn is held: rewrite the flagged
    // claim's lines and log the sessions it read; then the run commits.
    async Land(c) {
      const r = c.held[0];
      write(path.join(c.wiki, PAGE), pageBody(++c.version));
      const day = new Date().toISOString().slice(0, 10);
      for (const id of r.ids) fs.appendFileSync(path.join(c.wiki, 'log.md'), `\n## [${day}] ingest | ${id}#3 | ingested | ${PAGE}\n`);
      await end(c, true);
    },
    // runTimeout: the run ends having written nothing.
    Stop: (c) => end(c, false),
    async Poll(c) {
      await c.page.evaluate(() => { (window as unknown as { __visible?: boolean }).__visible = true; });
      const read = c.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/wiki');
      await c.page.clock.fastForward(60_000);
      await read;
      await c.page.evaluate(() => { (window as unknown as { __visible?: boolean }).__visible = false; });
    },
  },

  // The missing page and the dangling citation are 404s, a decision on
  // lines an ingest moved is a 409: the page shows each as a state.
  expectedErrors: [/^Failed to load resource: the server responded with a status of (404 \(Not Found\)|409 \(Conflict\))$/],

  read: readUiState,
  status: nav,
  // The ingest runs' headless sessions, for the trace check.
  sessions: (c) => {
    try {
      return fs.readdirSync(c.history).filter((f) => f.endsWith('.jsonl')).map((f) => f.slice(0, -6))
        .filter((id) => id !== SEED && !id.startsWith('pend-'));
    } catch { return []; }
  },
  async cleanup(c) {
    // Runs must be gone before serve's HOME is removed under them.
    while (c.held.length) await end(c, false);
  },
});

// What the walks cannot reach: the index's own Ingest now, offered only
// while the wiki has no pages yet. It dropped a refused start on the
// floor (`.catch(() => {})`), so the button did nothing visible while
// Activity's said why.
test.describe('wiki index: a refused ingest', () => {
  test('says why the ingest did not start', async ({ serve, page }) => {
    const bad: string[] = [];
    page.on('pageerror', (e) => bad.push(String(e)));
    page.on('console', (m) => { if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) bad.push(m.text()); });
    // Installed and waiting, no pages yet: the "Indexing" state.
    await page.route((u) => u.pathname === '/api/wiki', async (r) => {
      const ix = await (await r.fetch()).json();
      await r.fulfill({ json: { ...ix, exists: false, topics: [], health: { ...ix.health, installed: true, pending: 1 } } });
    });
    await page.route((u) => u.pathname === '/api/wiki/ingest', (r) =>
      r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'serve: api: start ingest: no bough binary' }) }));
    await page.goto(`${serve.url}/#/wiki`);
    await page.getByRole('button', { name: 'Ingest now', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('no bough binary');
    expect(bad).toEqual([]);
  });
});

// ToReview goes through the palette once nothing is flagged. The palette
// reset its query in an effect after opening, so the field it focused
// still held the last query and the walk's typed "Review flagged claims"
// was appended to it: only "Start a session" matched, and the click sent
// the doubled text as a first message. Opening is one keydown here, read
// back before any timer can run the deferred reset.
test.describe('palette: reopened after a query', () => {
  test('opens empty, before any key can land', async ({ serve, page }) => {
    await page.goto(`${serve.url}/#/wiki`);
    const field = page.locator('.pal-field');
    await expect(page.locator('.thread-head h1', { hasText: 'Wiki' })).toBeVisible();
    await page.keyboard.press('ControlOrMeta+k');
    await field.fill('Review flagged claims');
    await page.keyboard.press('Escape');
    await expect(field).toHaveCount(0);
    const value = await page.evaluate(async () => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', metaKey: true, bubbles: true }));
      for (let i = 0; i < 5; i++) await Promise.resolve();
      return (document.querySelector('.pal-field') as HTMLInputElement | null)?.value ?? 'no palette';
    });
    expect(value).toBe('');
  });
});
