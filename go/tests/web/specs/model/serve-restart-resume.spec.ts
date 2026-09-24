// go/tests/model/specs/serve_restart_resume.fizz in the browser (recipe:
// go/tests/model/README.md). The Room is one serve that the walk stops
// and starts on its HOME and port, the web session P a person has open
// on the page, and one background agent (the task) queued behind a
// running cap of 1 that another agent (h) holds with a blocked turn.
//
// The agents belong to a separate parent Q, open in a second tab: its
// Work button is where the page says whether the task is queued. Keeping
// them off P means a finished agent's report never wakes P's turn and
// takes one of the turns the walk queued for it.
//
// Each path gets a serve of its own (the per-test fixture, not the
// worker's): every path restarts it, and StartNew leaves it on another
// build, which no later test may inherit.
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  work: Page;     // the second tab, on Q: its Work button counts the agents
  serve: Serve;
  p: string;
  q: string;
  task: string;
  title: string;
  turn: number;
  held: string;   // P's turn in flight, '' when none
  hHeld: string;  // h's turn, '' once released or killed
  builds: number;
}

// The sidebar row: it shows P whatever the thread pane is doing. A
// pending ask also pins a copy under "Needs you"; both read the same.
const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.p}"]`).first();

// The row's accessible label is "<title>, <state>[, not seen yet], <age> ago".
const WORDS: Record<string, string> = { Running: 'running', 'Waiting for you': 'needs-you', Done: 'done', Interrupted: 'interrupted' };

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const r = row(c);
  const label = (await r.getAttribute('aria-label')) ?? '';
  const parts = label.split(', ');
  // What the page says about reaching serve: the list that stopped
  // refreshing, or the open thread's paused catch-up.
  const offline = (await c.page.locator('.side-fresh', { hasText: /Updates delayed|Sessions unavailable/ }).count()) > 0
    || (await c.page.locator('.rt-paused').count()) > 0;
  const banner = await c.page.locator('.updated', { hasText: 'bough updated' }).isVisible();
  let status = WORDS[parts[1]] ?? `unknown: ${label}`;
  let kids = (await r.getAttribute('data-live')) !== null ? 1 : 0;
  // Offline, every row is the last one the page saw, under "Updates
  // delayed": a row it saw running still says Running. What serve would
  // say now is not on the page, so the reader takes the page at its word
  // that serve is gone, and a gone serve leaves no child
  // (NoChildWithoutServe) and no live turn (InterruptedNeverRunning).
  // Once serve answers, both are read off the row again, which is the
  // check that the page catches up without a reload.
  if (offline) {
    kids = 0;
    if (status === 'running' || status === 'needs-you') status = 'interrupted';
  }
  const work = (await c.work.locator('button.work-summary').getAttribute('aria-label')) ?? '';
  // Queued while the Work button counts one; once it ran, the task is one
  // of the two workers that ended (h finished or stopped by the restart).
  const task = /\b1 queued\b/.test(work) ? 'queued' : /^Work, (2 finished|2 workers)\b/.test(work) ? 'started' : `unknown: ${work}`;
  return {
    serve: offline ? 'down' : 'up',
    page: offline ? 'offline' : 'live',
    status,
    kids,
    // The page names no build: it offers the reload exactly when the build
    // answering is not the one it loaded, so the banner is its word for it.
    build: banner ? 'new' : 'same',
    banner,
    task,
    titled: parts[0] === c.title,
  };
}

async function waitRow(c: Ctx, id: string, what: string, ok: (r: { status: string; agents?: { running: number; queued: number } }) => boolean): Promise<void> {
  const deadline = Date.now() + 20_000;
  let last = '';
  for (;;) {
    const res = await c.serve.api.get(`/api/sessions/${id}`);
    if (res.ok()) {
      const r = (await res.json()).session;
      if (ok(r)) return;
      last = JSON.stringify(r);
    }
    if (Date.now() > deadline) throw new Error(`${id} never ${what}: ${last}`);
    await new Promise((f) => setTimeout(f, 50));
  }
}

// Synchronisation, not state: waits until the task's child recorded its
// input and nothing but P runs, so no later turn the walk queues for P is
// taken by the task or by Q waking for an agent's report.
async function settle(c: Ctx): Promise<void> {
  const file = path.join(c.serve.home, '.bough', 'history', c.task + '.jsonl');
  const deadline = Date.now() + 20_000;
  for (;;) {
    const recorded = fs.existsSync(file) && fs.readFileSync(file, 'utf8').includes('"kind":"input"');
    const res = await c.serve.api.get('/api/sessions?all=1');
    if (recorded && res.ok()) {
      const rows = (await res.json()).sessions as { id: string; status: string; agents?: { running: number; queued: number } }[];
      const busy = rows.some((r) => (r.id !== c.p && (r.status === 'running' || r.status === 'queued'))
        || (r.id === c.q && r.agents && r.agents.running + r.agents.queued > 0));
      if (!busy) return;
    }
    if (Date.now() > deadline) throw new Error('sessions still busy after 20s');
    await new Promise((f) => setTimeout(f, 100));
  }
}

function nextTurn(c: Ctx, prefix: string): string {
  return `${prefix}${String(++c.turn).padStart(4, '0')}`;
}

async function post(c: Ctx, url: string, data: unknown): Promise<any> {
  const res = await c.serve.api.post(url, { data });
  if (!res.ok()) throw new Error(`${url}: ${res.status()} ${await res.text()}`);
  return res.json();
}

async function start(c: Ctx, bin?: string): Promise<void> {
  await c.serve.resume(bin);
  c.hHeld = '';
  await settle(c);
}

modelTests<Ctx>({
  spec: 'serve_restart_resume',
  role: 'Room#0',
  config: CONTROL_CONFIG,

  // Init is set up the way other clients would (the API, llm-control):
  // the page is only opened once the room is in the spec's first state.
  async init(page, serve) {
    // Three restarts and five children a path.
    test.setTimeout(90_000);
    const c: Ctx = { page, work: page, serve, p: '', q: '', task: '', title: 'room one', turn: 0, held: '', hHeld: '', builds: 0 };
    const dir = controlDir(serve.home);
    // Nothing queued: Q's first turn answers at once and is done.
    c.q = await serve.newSession("the agents' parent");
    await waitRow(c, c.q, 'done', (r) => r.status === 'done');

    const t = nextTurn(c, 't');
    queue(dir, t, { mode: 'block', text: `finished ${t}` });
    c.p = await serve.newSession(`turn ${t}`);
    c.held = t;
    await waitTaken(dir, t);
    await waitRow(c, c.p, 'running', (r) => r.status === 'running');
    await post(c, `/api/sessions/${c.p}/rename`, { title: c.title });

    const h = nextTurn(c, 'h');
    queue(dir, h, { mode: 'block', text: `held ${h}` });
    const hr = await post(c, '/api/sessions', { spawnedBy: c.q, prompt: `hold the slot ${h}`, maxRunning: 1, maxPerSession: 1 << 20 });
    if (hr.queued) throw new Error('the slot holder was queued');
    c.hHeld = h;
    await waitTaken(dir, h);
    const tr = await post(c, '/api/sessions', { spawnedBy: c.q, prompt: 'the queued task', maxRunning: 1, maxPerSession: 1 << 20 });
    if (!tr.queued) throw new Error('the task started past a running cap of 1');
    c.task = tr.session.id;

    c.work = await page.context().newPage();
    await c.work.goto(`${serve.url}/#/s/${c.q}`);
    await page.goto(`${serve.url}/#/s/${c.p}`);
    await page.bringToFront();
    return c;
  },

  actions: {
    // SIGTERM, as launchd and `bough update` send it. Not the page's.
    async Stop(c) {
      await c.serve.shutdown();
      c.held = '';
      c.hHeld = '';
    },
    // launchd KeepAlive starting the same binary again.
    Start: (c) => start(c),
    // `bough update`: the new binary at another path is another build
    // (buildID is the executable's path and mtime).
    async StartNew(c) {
      const bin = path.join(c.serve.home, `bough-new-${++c.builds}`);
      try {
        fs.linkSync(c.serve.bin(), bin);
      } catch {
        fs.copyFileSync(c.serve.bin(), bin);
        fs.chmodSync(bin, 0o755);
      }
      await start(c, bin);
    },
    // h's turn ends and frees the slot; the queue starts the task.
    async Drain(c) {
      release(controlDir(c.serve.home), c.hHeld);
      c.hHeld = '';
      await settle(c);
    },
    // P's held turn asks, the way the engine's tools.ask does.
    async Ask(c) {
      releaseWith(controlDir(c.serve.home), c.held, { mode: 'call', tool: 'ask', args: { question: 'go on?', options: ['yes', 'no'] } });
      c.held = '';
    },
    // The person picks an option on the page; the model's next request
    // is a held turn.
    async Answer(c) {
      const dir = controlDir(c.serve.home);
      const name = nextTurn(c, 't');
      queue(dir, name, { mode: 'block', text: `finished ${name}` });
      await c.page.locator('.ask .ask-option', { hasText: 'yes' }).click();
      c.held = name;
      await waitTaken(dir, name);
    },
    async Finish(c) {
      release(controlDir(c.serve.home), c.held);
      c.held = '';
    },
    // The composer: after a restart it is what gives P a child again.
    async Send(c) {
      const dir = controlDir(c.serve.home);
      const name = nextTurn(c, 't');
      queue(dir, name, { mode: 'block', text: `finished ${name}` });
      await c.page.locator('#composer').fill(`turn ${name}`);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      c.held = name;
      await waitTaken(dir, name);
    },
    Reload: (c) => c.page.locator('.updated').getByRole('button', { name: 'Reload', exact: true }).click(),
  },

  // Stop takes serve away under the open page on purpose; each request
  // the page makes meanwhile is refused, and Chromium logs every one.
  allowConsole: /^Failed to load resource: net::ERR_CONNECTION_REFUSED$/,

  read: readUiState,
  status: row,
  sessions: (c) => [c.p],
  async cleanup(c) {
    const dir = controlDir(c.serve.home);
    for (const n of [c.held, c.hHeld]) if (n) release(dir, n);
  },
});
