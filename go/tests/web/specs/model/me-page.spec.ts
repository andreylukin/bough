// go/tests/model/specs/me_page.fizz in the browser: every generated path
// through the Me page (#/me) walked against a real serve. The brief job is
// the real `bough wiki brief` the serve spawns, its one model turn held by
// llm-control, so the walk decides when the job ends and how. llm-control
// cannot call tools, so on BriefWritten the walk makes the agent's writes
// (today's brief, signals.json) before it releases the turn, exactly as
// the Go adapter (go/tests/model/mbt/me_page_test.go) does.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

// A path is up to ten steps, three of them brief jobs.
test.describe.configure({ timeout: 90_000 });

const TITLE = 'Review acme/web#7';
const CITE = 'gh:acme/web#7';

interface Ctx {
  page: Page;
  serve: Serve;
  held: string;          // the brief job's llm-control turn, '' when no job
  turn: number;
  daysBack: number;      // NextDay moves today's brief this many days back
  clickedAt: number;     // page clock at the last Refresh click
  first: Route | null;   // the page's first GET /api/me, held until Load or LoadFail
  hold: boolean;         // the next GET /api/me is held as first
  held1: () => void;     // resolves the wait for it
}

// Waits for the next GET /api/me the route holds.
const nextFirst = (c: Ctx) => new Promise<void>((r) => { c.first = null; c.hold = true; c.held1 = r; });

const meDir = (c: Ctx) => path.join(c.serve.home, '.bough', 'wiki', 'topics', 'me');
const logPath = (c: Ctx) => path.join(c.serve.home, '.bough', 'wiki', 'ingest.log');
const log = (c: Ctx) => { try { return fs.readFileSync(logPath(c), 'utf8'); } catch { return ''; } };
const today = () => { const d = new Date(); return ymd(d); };
const ymd = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;

// The bar when the page has data, the loading/error card before: one of
// the two carries the page's state at every node.
const carrier = (c: Ctx) => c.page.locator('.me-bar, .me-loading');
const primary = (c: Ctx) => c.page.locator('.me-bar button.btn-primary');

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const shown = (await p.locator('.me').count()) > 0;
  const failed = (await p.locator('.me-loading').getByText('Couldn’t read the brief').count()) > 0;
  const view = shown ? 'shown' : failed ? 'error' : 'loading';
  // Refresh is offered only with a profile; while the spinner runs it says so.
  const button = shown && (await primary(c).count()) > 0 ? ((await primary(c).textContent()) ?? '') : '';
  const brief = (await p.locator('article.me-brief').count()) === 0 ? 'none'
    : (await p.locator('.me-stale').count()) > 0 ? 'stale' : 'today';
  const signal = (await p.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).count()) > 0 ? 'pinned'
    : (await p.getByRole('button', { name: `Pin ${TITLE}`, exact: true }).count()) > 0 ? 'shown'
    : (await p.locator('.me-dismissed').count()) > 0 ? 'dismissed' : 'none';
  return {
    view,
    profile: button !== '',
    brief,
    job: (await p.locator('.me-job').count()) > 0 ? 'running' : 'idle',
    refreshing: button === 'Writing…',
    signal,
  };
}

// The server's own view of the job, for waits only: an action returns
// once the job the walk ended has really exited, so the state read after
// it is not racing a process still on its way out.
async function jobRunning(c: Ctx): Promise<boolean> {
  const res = await c.serve.api.get('/api/me');
  if (!res.ok()) throw new Error(`GET /api/me: ${res.status()}`);
  return Boolean((await res.json()).running);
}

async function waitFor(what: string, ok: () => Promise<boolean>, ms = 20_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`${what} after ${ms}ms`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

// After a job ends the page needs one read to say so. The walk nudges the
// page clock a second at a time (a read is armed every couple of seconds
// while a job runs) rather than leaving it to the harness's 5 s steps,
// which would run the 20 s spinner out before the state is compared.
async function waitJobShownIdle(c: Ctx): Promise<void> {
  await waitFor('job still running on the server', async () => !(await jobRunning(c)));
  // Each nudge waits for the row to go, not a fixed pause: it is usually
  // gone within a few ms of the read the nudge armed.
  for (let i = 0; i < 6 && (await c.page.locator('.me-job').count()) > 0; i++) {
    await c.page.clock.fastForward(1_000);
    await c.page.locator('.me-job').first().waitFor({ state: 'detached', timeout: 150 }).catch(() => {});
  }
}

// What the brief agent writes: today's brief and signals.json with one row.
function writeBrief(c: Ctx): void {
  const briefs = path.join(meDir(c), 'briefs');
  fs.mkdirSync(briefs, { recursive: true });
  fs.writeFileSync(path.join(briefs, today() + '.md'), `# ${today()}\n\nReviewing acme/web#7 today. \`${CITE}\`\n`);
  const signals = {
    asOf: new Date().toISOString(),
    items: [{ kind: 'needs-you', source: 'github', title: TITLE, repo: 'acme/web', author: 'dependabot', cite: CITE }],
    sources: [{ name: 'github', ok: true }],
  };
  fs.writeFileSync(path.join(meDir(c), 'signals.json'), JSON.stringify(signals));
}

async function endJob(c: Ctx, written: boolean): Promise<void> {
  const dir = controlDir(c.serve.home);
  if (written) {
    writeBrief(c);
    release(dir, c.held);
  } else {
    releaseWith(dir, c.held, { mode: 'error', error: 'brief agent failed' });
  }
  c.held = '';
  await waitJobShownIdle(c);
}

async function steer(c: Ctx, text: string, section: string): Promise<void> {
  await c.page.getByRole('textbox', { name: 'Tell the brief what to watch or ignore' }).fill(text);
  await c.page.getByRole('button', { name: `Add to ${section}`, exact: true }).click();
  // The section is the action's answer, not state: checked here.
  await expect(c.page.getByText(`Filed under ${section}. The next brief reads it.`)).toBeVisible();
}

modelTests<Ctx>({
  spec: 'me_page',
  role: 'Me#0',
  config: CONTROL_CONFIG,

  // The page's first read is held at the network so the walk can decide
  // how it ends; every later read goes to the serve.
  async init(page, serve) {
    const c: Ctx = { page, serve, held: '', turn: 0, daysBack: 0, clickedAt: 0, first: null, hold: false, held1: () => {} };
    await page.route((u) => u.pathname === '/api/me', async (route) => {
      if (!c.hold || route.request().method() !== 'GET') return route.fallback();
      c.hold = false;
      c.first = route;
      c.held1();
    });
    const first = nextFirst(c);
    await page.goto(`${serve.url}/#/me`);
    await first;
    return c;
  },

  actions: {
    Load: (c) => c.first!.continue(),
    // A read that fails without a failed request: a proxy answering its
    // own page. (A 5xx would also log "Failed to load resource", which
    // the console-error invariant rightly refuses on a page that works.)
    async LoadFail(c) {
      await c.first!.fulfill({ status: 200, contentType: 'text/html', body: '<html>gateway timeout</html>' });
    },
    // Retry's read is the next one the route sees: held again.
    async Retry(c) {
      const first = nextFirst(c);
      await c.page.getByRole('button', { name: 'Retry', exact: true }).click();
      await first;
    },
    // The empty state's button opens the profile in the wiki page view;
    // it is edited and saved there, and the Me nav brings the page back.
    async WriteProfile(c) {
      await c.page.getByRole('button', { name: 'Write your profile', exact: true }).click();
      await c.page.getByRole('button', { name: 'Edit', exact: true }).first().click();
      const box = c.page.locator('#wk-body');
      await box.fill((await box.inputValue()) + '\nI own acme/web.\n');
      await c.page.getByRole('button', { name: 'Save', exact: true }).click();
      await expect(box).toHaveCount(0);
      await c.page.locator('a[href="#/me"]').first().click();
      await expect(primary(c)).toBeVisible();
    },
    // With no job running the click starts one whose model turn is held;
    // with one running, the new `bough wiki brief` loses the wiki lock and
    // exits, which the walk waits to see so no stray run is left behind.
    async Refresh(c) {
      const dir = controlDir(c.serve.home);
      const before = (log(c).match(/already running/g) ?? []).length;
      const name = c.held ? '' : `b${String(++c.turn).padStart(4, '0')}`;
      if (name) queue(dir, name, { mode: 'block', text: `brief ${name}` });
      c.clickedAt = await c.page.evaluate(() => Date.now());
      await primary(c).click();
      if (name) {
        c.held = name;
        await waitTaken(dir, name);
      } else {
        await waitFor('second brief run did not report the held lock', async () => (log(c).match(/already running/g) ?? []).length > before);
      }
      // The page reads /api/me once the POST answers; give that read a
      // moment in real time so the harness's first compare (5 s of page
      // clock) is not spent on it. A page that never shows the job fails
      // the compare, not this.
      await c.page.locator('.me-job').waitFor({ timeout: 3_000 }).catch(() => {});
    },
    // The spinner's 20 s, on the page's clock.
    async Settle(c) {
      const now = await c.page.evaluate(() => Date.now());
      await c.page.clock.fastForward(Math.max(0, c.clickedAt + 20_000 - now) + 100);
    },
    BriefWritten: (c) => endJob(c, true),
    BriefFailed: (c) => endJob(c, false),
    // Midnight: today's brief becomes an earlier day's; the page's 30 s
    // poll finds it.
    async NextDay(c) {
      const briefs = path.join(meDir(c), 'briefs');
      const past = new Date(); past.setDate(past.getDate() - ++c.daysBack);
      fs.renameSync(path.join(briefs, today() + '.md'), path.join(briefs, ymd(past) + '.md'));
      await c.page.clock.fastForward(30_000);
    },
    Pin: (c) => c.page.getByRole('button', { name: `Pin ${TITLE}`, exact: true }).click(),
    Unpin: (c) => c.page.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).click(),
    // The menu's author rule, so the rule half of the request runs too.
    async Dismiss(c) {
      await c.page.getByRole('button', { name: `Dismiss ${TITLE}`, exact: true }).click();
      await c.page.getByRole('menuitem', { name: 'Nothing from dependabot', exact: true }).click();
    },
    SteerWatch: (c) => steer(c, 'Keep an eye on the release train', 'Watch'),
    SteerNotMine: (c) => steer(c, 'Ignore the docs site', 'Not mine'),
  },

  read: readUiState,
  status: carrier,
  // The brief jobs' headless sessions, for the trace check.
  sessions: (c) => {
    try { return fs.readdirSync(path.join(c.serve.home, '.bough', 'history')).filter((f) => f.endsWith('.jsonl')).map((f) => f.slice(0, -6)); } catch { return []; }
  },
  async cleanup(c) {
    c.hold = false;
    if (!c.held) return;
    // The job must be gone before serve's HOME is removed under it.
    releaseWith(controlDir(c.serve.home), c.held, { mode: 'error', error: 'walk over' });
    c.held = '';
    await waitFor('brief job still running after the walk', async () => !(await jobRunning(c)));
  },
});

// What the walks cannot reach: a request the page makes that the server
// refuses. Both were silent (an unhandled rejection, and for Refresh a
// 20 s "Writing…" over nothing); the page must say so and settle.
test.describe('me page: refused requests', () => {
  const brief = `# ${today()}\n\nReviewing acme/web#7 today. \`${CITE}\`\n`;
  const signals = JSON.stringify({ items: [{ kind: 'needs-you', source: 'github', title: TITLE, cite: CITE }], sources: [] });
  test.use({ serveOpts: { home: {
    '.bough/wiki/topics/me/profile.md': '# Me\n\nI own acme/web.\n',
    [`.bough/wiki/topics/me/briefs/${today()}.md`]: brief,
    '.bough/wiki/topics/me/signals.json': signals,
  } } });

  // Only the refused request itself may log; nothing may go unhandled.
  const watch = (page: Page) => {
    const bad: string[] = [];
    page.on('pageerror', (e) => bad.push(String(e)));
    page.on('console', (m) => { if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) bad.push(m.text()); });
    return bad;
  };

  test('a refresh the server refuses says why and stops writing', async ({ serve, page }) => {
    const bad = watch(page);
    await page.route((u) => u.pathname === '/api/me/refresh', (r) =>
      r.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'serve: api: start brief: no bough binary' }) }));
    await page.goto(`${serve.url}/#/me`);
    await page.getByRole('button', { name: 'Refresh', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('no bough binary');
    await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeEnabled();
    expect(bad).toEqual([]);
  });

  test('a triage the server refuses says why and leaves the row as it was', async ({ serve, page }) => {
    const bad = watch(page);
    await page.route((u) => u.pathname === '/api/me/triage', (r) =>
      r.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ error: 'wiki: triage: bad key' }) }));
    await page.goto(`${serve.url}/#/me`);
    await page.getByRole('button', { name: `Pin ${TITLE}`, exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('bad key');
    await expect(page.getByRole('button', { name: `Pin ${TITLE}`, exact: true })).toBeVisible();
    expect(bad).toEqual([]);
  });
});
