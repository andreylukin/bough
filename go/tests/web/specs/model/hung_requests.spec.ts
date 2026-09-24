// go/tests/model/specs/hung_requests.fizz in the browser (recipe:
// go/tests/model/README.md): the list poll, a send and Stop on one open
// thread whose requests may answer, fail or never answer. The Go twin is
// go/tests/model/mbt/hung_requests_test.go, which plays the page's
// refresh()/deliverTo/stop bookkeeping itself; here the page does it.
//
// The network is the walk's. Every list read (GET /api/sessions) and
// every send (POST /prompt) the page makes is held at the route until the
// spec's step says what the server does with it: answers (the request
// goes on to serve and its answer comes back), fails (a 200 whose body is
// not JSON: req() throws as on any failure, and Chromium logs nothing) or
// hangs (held until the walk ends). A list read the page supersedes while
// it is held and not hung is answered then: a slow read still answers,
// its seq is stale, and the spec says so of it.
//
// What is read off the page, and what off the page's own requests:
//
//   delayed  the sidebar's "Updates delayed … Retry" line
//   running  the thread's sidebar row's state word
//   stop     the composer's Stop button says "Stopping", or the page's
//            /interrupt is held for StopProceeds
//   list     the newest list read the page sent: none (answered or failed)
//            | out | hung (the page cannot tell hung from slow; that it
//            shows nothing is the spec's ListStaleShown gap)
//   send     the page's POST /prompt still held (post | posthung), then
//            the list read the page sent once it settled (read | readhung)
//   skipped  a poll tick after which the page sent no list read, until the
//            newest read lands. Nothing on screen says so.
//
// Poll ticks are the walk's too. The tab reads as hidden (document.hidden
// is stubbed) except inside PollTick, so the interval, which skips hidden
// tabs, runs only when the spec says; PollTick moves the page's clock one
// interval (12 s while a session is open) with the tab visible. Hidden
// also holds back the ack of a finish the page is showing, which would
// read the list on its own (see the flow's notes).
//
// StopProceeds is the page's own step and it takes it as soon as the
// sends it waits on settle: the /interrupt a waiting Stop sends is held
// at the route until StopProceeds.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

interface Req { route: Route; hung: boolean; settled: boolean }

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;        // llm-control's queue
  id: string;
  sends: number;
  holding: boolean;   // Init is over: every list read waits for the walk
  passing: number;    // Init's reads still on their way
  lists: Req[];       // every list read since Init, oldest first
  post?: Req;         // the send's POST /prompt
  sendRead?: Req;     // the list read deliverTo's finally sent
  skipped: boolean;
  deferStop: boolean; // the next /interrupt waits for StopProceeds
  stopRoute?: Route;
  idleStop: boolean;  // the /interrupt going out reaches a turn that is over
}

// The interval while a session's stream is open (app.tsx POLL_MS * 3).
const POLL_MS = 12_000;
const bound = 10_000;

let turns = 0;

// Three block turns always queued: every model request the session makes
// (the first turn, one a send starts, one a steer carries on) is held
// until the walk answers it.
function topUp(c: Ctx): void {
  fs.mkdirSync(c.dir, { recursive: true });
  const queued = fs.readdirSync(c.dir).filter((f) => f.endsWith('.json')).length;
  for (let n = queued; n < 3; n++) queue(c.dir, `h${String(++turns).padStart(8, '0')}`, { mode: 'block', text: 'answered' });
}

function releaseAll(c: Ctx): void {
  const held = fs.readdirSync(c.dir).filter((f) => f.endsWith('.taken')).map((f) => f.slice(0, -'.taken'.length))
    .filter((n) => !fs.existsSync(path.join(c.dir, n + '.release')));
  for (const n of held) release(c.dir, n);
  topUp(c);
}

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = bound): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`hung_requests: waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

interface Row { status: string }
interface Line { kind: string; text?: string }
async function session(c: Ctx): Promise<{ session: Row; entries: Line[] }> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  if (!res.ok()) throw new Error(`session: ${res.status()} ${await res.text()}`);
  return res.json();
}
const serverRunning = async (c: Ctx) => (await session(c)).session.status === 'running';

// A request's answer from serve, handed to the page.
async function answer(r: Req): Promise<void> {
  r.settled = true;
  try {
    const res = await r.route.fetch();
    await r.route.fulfill({ response: res });
  } catch { /* the page left */ }
}
async function fail(r: Req): Promise<void> {
  r.settled = true;
  await r.route.fulfill({ status: 200, contentType: 'application/json', body: 'not json: request failed' }).catch(() => {});
}

// The next list read the page sends after `n` have been seen.
const nextRead = (c: Ctx, n: number, what: string) => until(what, () => c.lists.length > n).then(() => c.lists[c.lists.length - 1]);
const newest = (c: Ctx) => c.lists[c.lists.length - 1];

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const label = (await row(c).getAttribute('aria-label')) ?? '';
  const n = newest(c);
  const list = !n || n.settled ? 'none' : n.hung ? 'hung' : 'out';
  let send = 'none';
  if (c.post && !c.post.settled) send = c.post.hung ? 'posthung' : 'post';
  else if (c.sendRead && !c.sendRead.settled) send = c.sendRead.hung ? 'readhung' : 'read';
  return {
    list,
    skipped: c.skipped,
    delayed: (await page.locator('.sidebar .side-fresh', { hasText: 'Updates delayed' }).getByRole('button', { name: 'Retry' }).count()) > 0,
    send,
    running: label.split(', ')[1] === 'Running',
    // The page's own StopProceeds is held with its /interrupt (above):
    // with nothing live it has already put its Stop button away.
    stop: c.stopRoute || (await page.locator('button.composer-stop[aria-label="Stopping"]').count()) > 0 ? 'waiting' : 'none',
  };
}

// A list read the page's poll or Retry sends lands.
async function land(c: Ctx, how: (r: Req) => Promise<void>): Promise<void> {
  const r = newest(c);
  if (!r || r.settled || r.hung) throw new Error('hung_requests: no list read out to land');
  await how(r);
  c.skipped = false;
}

// Stop's POST went out and the turn is over on the server; act's list
// read follows.
async function stopped(c: Ctx, n: number): Promise<void> {
  await until('the interrupted turn to end', async () => !(await serverRunning(c)));
  releaseAll(c);
  await nextRead(c, n, "act's list read after /interrupt");
}

modelTests<Ctx>({
  spec: 'hung_requests',
  role: 'Page#0',
  config: CONTROL_CONFIG,
  // PollTick is an action: a read must not run the poll.
  pollStepMs: 0,
  expectedError: (c, text) => c.idleStop && /Failed to load resource: .* 4\d\d/.test(text),

  async init(page, serve) {
    // The longest walks are ~50 steps.
    test.setTimeout(240_000);
    const c: Ctx = {
      page, serve, dir: controlDir(serve.home), id: '', sends: 0, holding: false, passing: 0,
      lists: [], skipped: false, deferStop: false, idleStop: false,
    };
    topUp(c);
    c.id = await serve.newSession('first turn');
    await until('the first turn to run', () => serverRunning(c));

    await page.addInitScript(() => {
      const w = window as unknown as { __hidden: boolean };
      w.__hidden = false;
      // List reads the page asked for, counted as fetch is called.
      const f = window.fetch.bind(window);
      (window as unknown as { __lists: number }).__lists = 0;
      window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
        const u = new URL(String(input instanceof Request ? input.url : input), location.href);
        if (u.pathname === '/api/sessions' && (init?.method ?? 'GET') === 'GET') (window as unknown as { __lists: number }).__lists++;
        return f(input, init);
      };
      Object.defineProperty(Document.prototype, 'hidden', { configurable: true, get: () => w.__hidden });
      Object.defineProperty(Document.prototype, 'visibilityState', { configurable: true, get: () => (w.__hidden ? 'hidden' : 'visible') });
    });
    await page.route((u) => u.pathname === '/api/sessions', async (route) => {
      if (route.request().method() !== 'GET') return route.fallback();
      const r: Req = { route, hung: false, settled: false };
      if (!c.holding) {
        c.passing++;
        await answer(r);
        c.passing--;
        return;
      }
      // A new read supersedes the ones still out: a slow one answers.
      const older = c.lists.filter((o) => !o.settled && !o.hung);
      c.lists.push(r);
      for (const o of older) await answer(o);
    });
    await page.route((u) => u.pathname === `/api/sessions/${c.id}/prompt`, (route) => {
      c.post = { route, hung: false, settled: false };
    });
    await page.route((u) => u.pathname === `/api/sessions/${c.id}/interrupt`, async (route) => {
      if (c.deferStop) { c.deferStop = false; c.stopRoute = route; return; }
      await answer({ route, hung: false, settled: false });
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await row(c).waitFor();
    await page.locator('#composer').waitFor();
    // Init's list is idle: the mount's reads have landed and the page is
    // quiet before the walk takes the network over.
    let quiet = 0;
    await until('the first list reads to land', async () => {
      quiet = c.passing === 0 ? quiet + 1 : 0;
      return quiet >= 20;
    });
    c.holding = true;
    await page.evaluate(() => { (window as unknown as { __hidden: boolean }).__hidden = true; });
    return c;
  },

  actions: {
    // The interval fires once, with the tab visible.
    async PollTick(c) {
      const n = c.lists.length;
      const calls = () => c.page.evaluate(() => (window as unknown as { __lists: number }).__lists);
      const before = await calls();
      await c.page.evaluate(() => { (window as unknown as { __hidden: boolean }).__hidden = false; });
      await c.page.clock.fastForward(POLL_MS);
      await c.page.evaluate(() => { (window as unknown as { __hidden: boolean }).__hidden = true; });
      // refresh() calls fetch before its first await, so the tick has
      // asked for the list by now or it never will.
      if ((await calls()) === before) c.skipped = true;
      else await nextRead(c, n, 'the poll read');
    },

    ListAnswers: (c) => land(c, answer),
    ListFails: (c) => land(c, fail),
    async ListHangs(c) {
      const r = newest(c);
      if (!r || r.settled || r.hung) throw new Error('hung_requests: no list read out to hang');
      r.hung = true;
    },

    async RetryList(c) {
      const n = c.lists.length;
      await c.page.locator('.sidebar .side-fresh').getByRole('button', { name: 'Retry' }).click();
      await nextRead(c, n, "Retry's list read");
    },

    async Send(c) {
      // The page took StopProceeds when the sends settled; the walk only
      // holds its /interrupt. act() is busy until it answers, and busy
      // disables the composer: a Send the spec allows in between is one
      // the page cannot make.
      if (c.stopRoute) test.skip(true, 'Send while the page\'s /interrupt is out: act() keeps the composer busy');
      c.post = undefined;
      c.sendRead = undefined;
      await c.page.locator('#composer').fill(`send ${++c.sends}`);
      await c.page.getByRole('button', { name: /^(Send|Steer)$/ }).click();
      await until('the send to reach the network', () => c.post !== undefined);
    },

    // The POST reaches serve: a steer while the turn runs, a new turn
    // otherwise. Then deliverTo's finally reads the list.
    async SendPostAnswers(c) {
      const was = await serverRunning(c);
      topUp(c);
      const n = c.lists.length;
      await answer(c.post!);
      const text = `send ${c.sends}`;
      if (was) {
        // The steer is in the running turn before anything can finish it.
        await until("the steer's input", async () => (await session(c)).entries.some((l) => l.kind === 'input' && l.text?.trim() === text));
      } else {
        await until("the sent prompt's turn", () => serverRunning(c));
      }
      c.sendRead = await nextRead(c, n, "the send's list read");
    },
    async SendPostFails(c) {
      const n = c.lists.length;
      await fail(c.post!);
      c.sendRead = await nextRead(c, n, "the send's list read");
    },
    async SendPostHangs(c) { c.post!.hung = true; },

    async Stop(c) {
      const waits = Boolean((c.post && !c.post.settled) || (c.sendRead && !c.sendRead.settled));
      c.deferStop = waits;
      c.idleStop = false;
      const n = c.lists.length;
      await c.page.getByRole('button', { name: 'Stop', exact: true }).click();
      if (!waits) await stopped(c, n);
    },

    async StopProceeds(c) {
      await until('the waiting Stop to POST /interrupt', () => c.stopRoute !== undefined);
      c.idleStop = !(await serverRunning(c));
      const n = c.lists.length;
      const r = c.stopRoute!;
      c.stopRoute = undefined;
      await answer({ route: r, hung: false, settled: false });
      await stopped(c, n);
    },

    // The model's reply ends the turn (and whatever a steer carried on).
    async Finish(c) {
      await until('the turn to end', async () => {
        if (!(await serverRunning(c))) return true;
        releaseAll(c);
        return false;
      });
    },
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    c.holding = false;
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
    releaseAll(c);
  },
});
