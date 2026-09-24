// go/tests/model/specs/ui_me.fizz walked in the browser (recipe:
// go/tests/model/README.md): the Me page's first read, the empty
// profile, Refresh against a real `bough wiki brief` run whose model is
// llm-control, the steering line, and triage of the one signal row.
// Every generated path is one test against the worker's serve; at each
// node readUiState must equal the spec's Me#0 state, and the screen must
// pass axe, show no clipped header or status text, and ring whatever has
// keyboard focus.
//
// The page decides nothing about when its reads answer, so the walk
// does, at the network:
// - Every GET /api/me is held unless a step the spec says reads is
//   running: Poll (PollFail fails the held read instead), RefreshSettle,
//   and a triage, whose answer the page reloads after. The page's clock
//   runs, so its 30 s poll may ask at any time; asked outside those
//   steps, the read waits and is the next Poll's.
// - The answer to POST /api/me/refresh is held until RefreshSettle. The
//   run starts at once (the request reaches serve), but the page arms
//   its 20 s "Writing…" only on the answer, so under any load it cannot
//   end before the step that ends it. RefreshSettle then moves the clock
//   past it; Poll moves it past the poll.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, releaseWith, waitTaken, type Turn } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';
import { axeClean, focusRingVisible, noClippedText } from '../../helpers/ui-invariants';

// A path runs up to three brief runs, each a real headless session.
test.describe.configure({ timeout: 120_000 });

const TITLE = 'Review acme/app#1';
const KEY = 'gh:acme/app#1';
const POLL_MS = 30_000;    // MeView's useLoad poll
const SETTLE_MS = 20_000;  // refresh()'s "Writing…" timer

// What the brief run writes, as its bash call: today's brief and one
// signal row. `date` is the run's own clock, the one serve dates by.
const WRITE_BRIEF = [
  'mkdir -p topics/me/briefs',
  `printf '# Brief\\n\\n## Today\\n\\n- Ship the Me walk.\\n' > "topics/me/briefs/$(date +%F).md"`,
  `printf '%s\\n' '${JSON.stringify({
    items: [{ kind: 'needs-you', source: 'gh', title: TITLE, cite: KEY, repo: 'acme/app', author: 'octo' }],
    sources: [{ name: 'gh', ok: true }],
  })}' > topics/me/signals.json`,
].join('\n');

interface Ctx {
  page: Page;
  serve: Serve;
  me: string;        // ~/.bough/wiki/topics/me
  ctrl: string;
  held: Route[];     // GET /api/me not yet let through
  open: boolean;     // a step that reads is running: reads go through
  answer: (() => Promise<void>) | null;  // hands the page the held refresh answer
  turn: string;      // the brief run's held llm-control turn, '' when none
  failing: boolean;  // SteerFail's 500 is this step's own console error
}

// One queue per worker's serve: names only grow, so a turn left from an
// earlier path can never be taken ahead of this one's.
let seq = 0;

const today = () => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
const profile = (c: Ctx) => path.join(c.me, 'profile.md');
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until(what: string, ok: () => Promise<boolean> | boolean, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`ui_me: ${what}`);
    await sleep(25);
  }
}

// `bough wiki brief` is serve's own child while it runs.
function running(c: Ctx): boolean {
  try {
    return execFileSync('pgrep', ['-P', String(c.serve.pid), '-f', 'wiki brief'], { encoding: 'utf8' }).trim() !== '';
  } catch {
    return false;
  }
}

const runEnded = (c: Ctx) => until('the brief run did not exit', () => !running(c), 20_000);

// The server's half, read where serve keeps it; the brief run by its
// process, since no page shows one.
function serverState(c: Ctx) {
  let srv_signal = 'none';
  if (fs.existsSync(path.join(c.me, 'signals.json'))) {
    srv_signal = 'shown';
    const t = path.join(c.me, 'triage.json');
    if (fs.existsSync(t)) {
      const tri = JSON.parse(fs.readFileSync(t, 'utf8')) as { dismissed?: Record<string, string>; pinned?: string[] };
      if (tri.pinned?.includes(KEY)) srv_signal = 'pinned';
      else if (tri.dismissed && KEY in tri.dismissed) srv_signal = 'dismissed';
    }
  }
  return {
    srv_profile: fs.existsSync(profile(c)),
    srv_brief: fs.existsSync(path.join(c.me, 'briefs', today() + '.md')) ? 'today' : 'none',
    srv_signal,
    run: running(c) ? 'running' : 'idle',
  };
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const loaded = (await page.locator('.me').count()) > 0;
  let data = 'loading';
  if (loaded) data = 'loaded';
  else if (await page.locator('.me-loading h2', { hasText: /^Couldn’t read the brief$/ }).count()) data = 'error';

  let signal = 'none';
  if (await page.locator('.me-sig-wrap').count()) signal = (await page.locator('section.me-group[aria-label="Pinned"] .me-sig-wrap').count()) ? 'pinned' : 'shown';
  else if (await page.locator('.me-dismissed').count()) signal = 'dismissed';

  let steer = '';
  if (await page.locator('.me-steer [role=status]').count()) steer = 'said';
  else if (await page.locator('.me-steer [role=alert]').count()) steer = 'error';

  return {
    ...serverState(c),
    data,
    profile: loaded && (await page.locator('.me-body h2', { hasText: /^Tell the brief whose work this is$/ }).count()) === 0,
    brief: (await page.locator('article.me-brief').count()) ? 'today' : 'none',
    signal,
    refreshing: (await page.locator('.me-bar button', { hasText: /^Writing…$/ }).count()) > 0,
    menu: (await page.getByRole('menu', { name: 'Dismiss' }).count()) > 0,
    steer,
  };
}

// The page shows what the server has: every read's postcondition.
async function caughtUp(c: Ctx): Promise<boolean> {
  const s = await readUiState(c);
  return s.data === 'loaded' && !s.refreshing
    && s.profile === s.srv_profile && s.brief === s.srv_brief && s.signal === s.srv_signal;
}

// Reads go through while `step` runs, the held ones first, until the
// page has caught up with the server. A read still in flight after that
// can only show the same thing.
async function reading(c: Ctx, step: () => Promise<void>): Promise<void> {
  c.open = true;
  try {
    for (const r of c.held.splice(0)) await r.continue();
    await step();
    await until('the page never caught up with the server', () => caughtUp(c));
  } finally {
    c.open = false;
  }
}

// The steering box sends on Enter, as a person types it.
async function steer(c: Ctx, text: string): Promise<number> {
  const box = c.page.getByRole('textbox', { name: 'Tell the brief what to watch or ignore' });
  const answered = c.page.waitForResponse((r) => r.url().endsWith('/api/me/steer'));
  // After a failure the sentence is still there: sending it again is the retry.
  if (!(await box.inputValue())) {
    await box.click();
    await c.page.keyboard.type(text);
  } else {
    await box.focus();
  }
  await c.page.keyboard.press('Enter');
  return (await answered).status();
}

// A triage answer, and the page's reload after it.
async function triage(c: Ctx, click: () => Promise<void>): Promise<void> {
  await reading(c, async () => {
    const answered = c.page.waitForResponse((r) => r.url().endsWith('/api/me/triage'));
    await click();
    await answered;
  });
}

modelTests<Ctx>({
  spec: 'ui_me',
  role: 'Me#0',
  config: CONTROL_CONFIG,
  shared: true,
  // Poll is an action here: a read must not move the clock past it.
  pollStepMs: 0,

  async init(page, serve) {
    const c: Ctx = {
      page, serve,
      me: path.join(serve.home, '.bough', 'wiki', 'topics', 'me'),
      ctrl: controlDir(serve.home),
      held: [], open: false, answer: null, turn: '', failing: false,
    };
    // The worker's serve back to the spec's Init: no profile, no brief,
    // no signals, nothing triaged, no run.
    await runEnded(c);
    for (const f of ['profile.md', 'signals.json', 'triage.json', `briefs/${today()}.md`]) fs.rmSync(path.join(c.me, f), { force: true });
    await page.route('**/api/me', (r) => {
      if (r.request().method() === 'GET' && !c.open) c.held.push(r);
      else void r.continue().catch(() => {});
    });
    await page.route('**/api/me/refresh', async (r) => {
      const res = await r.fetch();
      c.answer = () => r.fulfill({ response: res });
    });
    await page.goto(`${serve.url}/#/me`);
    return c;
  },

  actions: {
    async Poll(c) {
      c.failing = false;
      // The first read (or Retry's) is on its way by itself; any other
      // is the poll's, moved up.
      const { data } = await readUiState(c);
      await until('no read of /api/me to answer', async () => {
        if (c.held.length) return true;
        if (data !== 'loading') await c.page.clock.fastForward(POLL_MS);
        return false;
      });
      await reading(c, async () => {});
    },
    async PollFail(c) {
      c.failing = false;
      await until('no read of /api/me to fail', () => c.held.length > 0);
      // A 200 that is not JSON fails req() like a 500 would, without
      // the console line Chromium logs for every non-2xx fetch.
      await c.held.shift()!.fulfill({ status: 200, contentType: 'application/json', body: 'not json: read failed' });
    },
    async Retry(c) {
      c.failing = false;
      await c.page.getByRole('button', { name: 'Retry' }).click();
    },
    async WriteProfile(c) {
      c.failing = false;
      // "Write your profile" routes to the wiki editor, which can only
      // save a page that exists: the profile is written by an editor or
      // an agent, as another client would. The page learns it on a read.
      fs.mkdirSync(c.me, { recursive: true });
      fs.writeFileSync(profile(c), '# Me\n\nI review acme/app.\n');
    },
    async Refresh(c) {
      c.failing = false;
      c.turn = `me-${String(++seq).padStart(5, '0')}`;
      queue(c.ctrl, c.turn + '-a', { mode: 'block', text: 'held' });
      // With no brief yet the empty state offers "Write it now"; with
      // one it is the bar's Refresh. Both start the same run.
      const noBrief = c.page.locator('.me-none').getByRole('button', { name: 'Write it now' });
      const button = (await noBrief.count()) ? noBrief : c.page.locator('.me-bar').getByRole('button', { name: 'Refresh' });
      await button.click();
      await until('POST /api/me/refresh never reached serve', () => c.answer !== null);
      await waitTaken(c.ctrl, c.turn + '-a');
    },
    async RefreshSettle(c) {
      c.failing = false;
      await reading(c, async () => {
        const answered = c.page.waitForResponse((r) => r.url().endsWith('/api/me/refresh'));
        await c.answer!();
        c.answer = null;
        await answered;
        // The timer is armed once the page has handled the answer.
        await until('"Writing…" never ended', async () => {
          await c.page.clock.fastForward(SETTLE_MS);
          return (await c.page.locator('.me-bar button', { hasText: /^Writing…$/ }).count()) === 0;
        });
      });
    },
    async BriefDone(c) {
      c.failing = false;
      queue(c.ctrl, c.turn + '-b', { mode: 'ok', text: 'Brief written.' });
      const write: Turn & { bash: string } = { mode: 'ok', bash: WRITE_BRIEF };
      releaseWith(c.ctrl, c.turn + '-a', write);
      await runEnded(c);
      c.turn = '';
    },
    async BriefFail(c) {
      c.failing = false;
      releaseWith(c.ctrl, c.turn + '-a', { mode: 'error', error: 'a source is down' });
      await runEnded(c);
      c.turn = '';
    },
    async SteerOk(c) {
      c.failing = false;
      const status = await steer(c, 'watch the release train');
      if (status !== 200) throw new Error(`ui_me: steer answered ${status}`);
    },
    async SteerFail(c) {
      // The profile cannot be written: serve's steer is a 500.
      c.failing = true;
      fs.chmodSync(profile(c), 0o444);
      try {
        const status = await steer(c, 'ignore dependabot');
        if (status !== 500) throw new Error(`ui_me: steer answered ${status}, want 500`);
      } finally {
        fs.chmodSync(profile(c), 0o644);
      }
    },
    async Pin(c) {
      c.failing = false;
      await triage(c, () => c.page.getByRole('button', { name: `Pin ${TITLE}` }).click());
    },
    async Unpin(c) {
      c.failing = false;
      await triage(c, () => c.page.getByRole('button', { name: `Unpin ${TITLE}` }).click());
    },
    async MenuToggle(c) {
      c.failing = false;
      // From the keyboard: the ✕ keeps focus, so the node after it is
      // where the focus ring is owed.
      await c.page.getByRole('button', { name: `Dismiss ${TITLE}` }).focus();
      await c.page.keyboard.press('Enter');
    },
    async Dismiss(c) {
      c.failing = false;
      await triage(c, () => c.page.getByRole('menuitem', { name: 'Just this one' }).click());
    },
  },

  read: readUiState,
  status: (c) => c.page.locator('.me-bar, .me-loading'),
  sessions: () => [],

  expectedError: (c, text) => c.failing && /status of 500/.test(text),

  async check(c, where) {
    const root = (await c.page.locator('.me').count()) ? '.me' : '.me-loading';
    await axeClean(c.page, root, where);
    await noClippedText(c.page, '.me-bar, .me-group-h, .me-body .empty-state h2, .me-loading h2, .me-steer [role=status], .me-steer [role=alert]', where);
    await focusRingVisible(c.page, where);
  },

  async cleanup(c) {
    if (!c) return;
    if (fs.existsSync(profile(c))) fs.chmodSync(profile(c), 0o644);
    if (c.turn) releaseWith(c.ctrl, c.turn + '-a', { mode: 'error', error: 'walk over' });
    await runEnded(c).catch(() => {});
  },
});
