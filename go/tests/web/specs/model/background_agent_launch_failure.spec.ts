// go/tests/model/specs/background_agent_launch_failure.fizz in the browser
// (recipe: go/tests/model/README.md): a parent's background agents whose
// start goes wrong, as its Work dialog and transcript show them. Every
// generated path is walked against a real serve whose model is
// llm-control, and at each node readUiState must equal the spec's Parent.
//
// The faults are the Go adapter's (mbt/background_agent_launch_failure_test.go),
// real, not mocked:
//   - launch: serve runs from its own hard link of the bough binary, and
//     BreakLaunch renames that link away, so exec of a child fails.
//   - hang: llm-control's start hold parks every child that mounts the
//     llm row, before its history: a live process with no history file.
//   - KillA/KillB: SIGKILL to the parked child (start-<pid>.held).
//   - ServeRestart: SIGTERM and a start of the same link on the same HOME.
//
// The parent is a session with no process, as in background_agents: a
// live parent would take every report as a wake turn. The two agents are
// told apart by title, so the titler is off.
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { expect, type Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

const MAX_RUNNING = 1;
const MAX_PER_SESSION = 1 << 20;
const A = 'task a';
const B = 'task b';

type Status = 'none' | 'queued' | 'booting' | 'running' | 'done' | 'failed' | 'stopped';

// The spec's state as the walk drove it: it says which child a freed slot
// starts, so the walk queues that child's model turn before the step.
// readUiState never reports it.
interface Model {
  fault: string;
  a: Status; b: Status;
  aSlot: boolean; bSlot: boolean;
}

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;      // llm-control's queue and start hold
  exe: string;      // serve's own link of the binary
  parent: string;
  a: string; b: string;
  answer: string;   // what the last spawn call answered
  aTurn: string; bTurn: string;
  turn: number;
  queued: string[]; // every turn queued, for cleanup
  m: Model;
  restarting: boolean;
}

// ---- the machine ----

function nextTurn(c: Ctx): string {
  const name = `l${String(++c.turn).padStart(5, '0')}`;
  queue(c.dir, name, { mode: 'block', text: `finished ${name}` });
  c.queued.push(name);
  return name;
}

function holdStart(c: Ctx): void {
  fs.mkdirSync(c.dir, { recursive: true });
  fs.rmSync(path.join(c.dir, 'start.exit'), { force: true });
  for (const f of fs.readdirSync(c.dir)) if (/^start-\d+\.held$/.test(f)) fs.rmSync(path.join(c.dir, f), { force: true });
  fs.writeFileSync(path.join(c.dir, 'start.hold'), '');
}
const releaseStart = (c: Ctx) => fs.rmSync(path.join(c.dir, 'start.hold'), { force: true });

function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch { return false; }
}

// The live children parked at the start hold; a SIGKILLed one never
// removed its marker, so dead markers are dropped.
function heldPids(c: Ctx): number[] {
  const pids: number[] = [];
  for (const f of fs.existsSync(c.dir) ? fs.readdirSync(c.dir) : []) {
    const m = /^start-(\d+)\.held$/.exec(f);
    if (!m) continue;
    if (alive(Number(m[1]))) pids.push(Number(m[1]));
    else fs.rmSync(path.join(c.dir, f), { force: true });
  }
  return pids;
}

async function until<T>(what: string, get: () => T | Promise<T>, ok: (v: T) => boolean, ms = 20_000): Promise<T> {
  for (const end = Date.now() + ms; ; await new Promise((r) => setTimeout(r, 50))) {
    const v = await get();
    if (ok(v)) return v;
    if (Date.now() > end) throw new Error(`background_agent_launch_failure: ${what} after ${ms}ms`);
  }
}

// A parent with no process: its history file and meta line.
function writeParent(home: string, cwd: string): string {
  const id = crypto.randomUUID();
  const file = path.join(home, '.bough', 'history', `${id}.jsonl`);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify({ seq: 1, at: new Date().toISOString(), kind: 'meta', data: { cwd } }) + '\n');
  return id;
}

function meta(c: Ctx): Record<string, { spawnedBy?: string; task?: unknown }> {
  const file = path.join(c.serve.home, '.bough', 'serve', 'meta.json');
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8')).sessions ?? {};
  } catch {
    return {};
  }
}

// ---- the spec's transitions, for the turns a step starts ----

const started = (fault: string): Status => (fault === 'launch' ? 'failed' : fault === 'hang' ? 'booting' : 'running');

// The spec's drain after a slot frees: the queue starts, a first. A child
// it starts on a sound machine opens its model call at once, so that
// turn is queued before the step that frees the slot.
function drainNext(c: Ctx): void {
  const m = c.m;
  let busy = m.aSlot || m.bSlot;
  if (m.a === 'queued' && !busy) {
    m.a = started(m.fault);
    if (m.a !== 'failed') { m.aSlot = busy = true; }
    if (m.a === 'running') c.aTurn = nextTurn(c);
  }
  if (m.b === 'queued' && !busy) {
    m.b = started(m.fault);
    if (m.b !== 'failed') m.bSlot = true;
    if (m.b === 'running') c.bTurn = nextTurn(c);
  }
}

// ---- the page ----

const dialog = (c: Ctx) => c.page.getByRole('dialog', { name: /^Work/ });
const workButton = (c: Ctx) => c.page.locator('button.work-summary');
const agentRow = (c: Ctx, title: string) => dialog(c).locator('.work-row').filter({ has: c.page.getByRole('button', { name: title, exact: true }) });

const LIFE: Record<string, Status> = { running: 'running', queued: 'queued', finished: 'done', failed: 'failed', stopped: 'stopped' };

// Rows move between groups as agents end (Running, Needs review, the
// Finished fold), so the clicks are bounded: a row that moved away
// under a click is read again on the next poll instead of waiting on it.
async function openDialog(c: Ctx): Promise<boolean> {
  if (!(await workButton(c).count())) return false;
  if (!(await dialog(c).isVisible())) await workButton(c).click({ timeout: 2_000 });
  for (const fold of await dialog(c).locator('.work-group-toggle[aria-expanded="false"]').all()) await fold.click({ timeout: 2_000 });
  return true;
}

// A row's status as the spec names it, read in one pass over the DOM. A
// started agent that has not recorded anything yet says so in its word
// ("Starting"): that is the spec's booting.
async function lifeOf(c: Ctx, title: string): Promise<string> {
  const rows = await dialog(c).locator('.work-row').evaluateAll((els, t) => els
    .filter((e) => e.querySelector('.work-row-head')?.getAttribute('title') === t || e.querySelector('.work-row-text')?.textContent === t)
    .map((e) => ({ life: e.getAttribute('data-life') ?? '', word: e.querySelector('.work-row-meta .work-word')?.textContent?.trim() ?? '' })), title);
  if (!rows.length) return 'none';
  const { life, word } = rows[0];
  if (life === 'running' && word === 'Starting') return 'booting';
  return LIFE[life] ?? `unknown: ${life}`;
}

// The reports in the parent's transcript about the agent titled title.
async function notices(c: Ctx, title: string): Promise<number> {
  return c.page.locator('.agent-notice .agent-notice-name').evaluateAll(
    (els, t) => els.filter((e) => e.getAttribute('title') === t).length, title);
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const open = await openDialog(c);
  const a = open ? await lifeOf(c, A) : 'none';
  const b = open ? await lifeOf(c, B) : 'none';
  const total = open ? await dialog(c).locator('.work-row').count() : 0;
  // The Work button's "N running" is per parent: credited to the agent
  // that is booting or running, else (a leak) to one that holds none.
  const label = open ? (await workButton(c).getAttribute('aria-label')) ?? '' : '';
  let left = Number(/(\d+) running/.exec(label)?.[1] ?? 0);
  const slot: Record<string, boolean> = { a: false, b: false };
  const st: Record<string, string> = { a, b };
  for (const k of ['a', 'b']) if (left > 0 && (st[k] === 'booting' || st[k] === 'running')) { slot[k] = true; left--; }
  for (const k of ['a', 'b']) if (left > 0 && !slot[k] && st[k] !== 'none' && st[k] !== 'queued') { slot[k] = true; left--; }
  // Not on any page: the machine the agents start on (the walk's own
  // fault), the Task serve keeps on disk for a restart (meta.json), and
  // what the agent's spawn call answered (the tool's result, the walk's).
  // A spawn answered with an error has no id: any row of the parent's the
  // walk did not name is it.
  const rows = meta(c);
  let aId = c.a;
  if (!aId) for (const [id, r] of Object.entries(rows)) if (r.spawnedBy === c.parent && id !== c.b) aId = id;
  const task = (id: string) => !!id && rows[id]?.task != null;
  const fault = !fs.existsSync(c.exe) ? 'launch' : fs.existsSync(path.join(c.dir, 'start.hold')) ? 'hang' : 'none';
  return {
    fault, a, b, a_slot: slot.a, b_slot: slot.b, a_task: task(aId), b_task: task(c.b),
    a_n: await notices(c, A), b_n: await notices(c, B), total, answer: c.answer,
  };
}

// tools.spawn({background}) is the agent's call, not the person's: it
// has no UI, so it is the POST the tool makes.
async function spawn(c: Ctx, prompt: string): Promise<{ status: number; id: string; queued: boolean }> {
  const res = await c.serve.api.post('/api/sessions', {
    data: { cwd: c.serve.work, prompt, spawnedBy: c.parent, maxPerSession: MAX_PER_SESSION, maxRunning: MAX_RUNNING },
  });
  if (!res.ok()) return { status: res.status(), id: '', queued: false };
  const body = await res.json();
  return { status: res.status(), id: body.session.id, queued: !!body.queued };
}

async function childStatus(c: Ctx, id: string): Promise<string | undefined> {
  const res = await c.serve.api.get(`/api/sessions/${c.parent}/children`);
  if (!res.ok()) return undefined;
  return ((await res.json()).children as { id: string; status: string }[]).find((r) => r.id === id)?.status;
}

// Stop agent on a row of the Work dialog. A running agent's interrupt
// waits out serve's hold on an unread prompt (holdLimit, 20 s: the held
// llm-control turn prints nothing, see background_agents' Stop), so the
// wait is on the API, with the page's clock still.
async function stopRow(c: Ctx, title: string, id: string, was: Status): Promise<void> {
  if (was !== 'queued') drainNext(c);
  await openDialog(c);
  await agentRow(c, title).getByRole('button', { name: 'Stop agent' }).click();
  if (was === 'queued') return;
  await until('the stop to land', () => childStatus(c, id), (s) => s === 'stopped', 40_000);
}

async function kill(c: Ctx): Promise<void> {
  drainNext(c);
  const pids = await until('a child parked at the start hold', () => heldPids(c), (p) => p.length > 0);
  for (const pid of pids) process.kill(pid, 'SIGKILL');
}

// Stop waits out serve's 20 s interrupt hold, and a walk may stop twice.
test.describe.configure({ timeout: 180_000 });

modelTests<Ctx>({
  spec: 'background_agent_launch_failure',
  role: 'Parent#0',
  config: CONTROL_CONFIG + '- id: session-title\n  plugin: session-title\n  disabled: true\n',

  async init(page, serve) {
    // Serve children exec serve's own executable; the shared binary must
    // never go away under other tests, so serve runs from a link of its
    // own that BreakLaunch can rename.
    await serve.shutdown();
    const exe = path.join(serve.home, 'bough-blf');
    try {
      fs.linkSync(serve.bin(), exe);
    } catch {
      fs.copyFileSync(serve.bin(), exe);
      fs.chmodSync(exe, 0o755);
    }
    await serve.resume(exe);
    const c: Ctx = {
      page, serve, dir: controlDir(serve.home), exe, parent: writeParent(serve.home, serve.work),
      a: '', b: '', answer: '', aTurn: '', bTurn: '', turn: 0, queued: [],
      m: { fault: 'none', a: 'none', b: 'none', aSlot: false, bSlot: false }, restarting: false,
    };
    await page.goto(`${serve.url}/#/s/${c.parent}`);
    return c;
  },

  actions: {
    async BreakLaunch(c) {
      c.m.fault = 'launch';
      fs.renameSync(c.exe, c.exe + '.gone');
    },

    async BreakBoot(c) {
      c.m.fault = 'hang';
      holdStart(c);
    },

    // The machine is fixed; a parked child goes on and opens its turn,
    // which is held so it stays running.
    async Repair(c) {
      const was = c.m.fault;
      c.m.fault = 'none';
      if (was === 'launch') {
        fs.renameSync(c.exe + '.gone', c.exe);
        return;
      }
      if (c.m.a === 'booting') { c.aTurn = nextTurn(c); c.m.a = 'running'; }
      if (c.m.b === 'booting') { c.bTurn = nextTurn(c); c.m.b = 'running'; }
      releaseStart(c);
    },

    async SpawnA(c) {
      const st = started(c.m.fault);
      const name = st === 'running' ? nextTurn(c) : '';
      const r = await spawn(c, A);
      if (r.status >= 500) {
        c.answer = 'error';
      } else {
        expect(r.status, 'spawn a').toBe(201);
        expect(r.queued, 'a queued with the cap free').toBe(false);
        c.answer = 'ok';
        c.a = r.id;
        c.aTurn = name;
      }
      if (st !== 'failed') { c.m.a = st; c.m.aSlot = true; }
    },

    async SpawnB(c) {
      const r = await spawn(c, B);
      expect(r.status, 'spawn b').toBe(201);
      expect(r.queued, 'b queued behind a running cap of 1').toBe(true);
      c.b = r.id;
      c.answer = 'ok';
      c.m.b = 'queued';
    },

    async FinishA(c) {
      c.m.a = 'done';
      c.m.aSlot = false;
      drainNext(c);
      release(c.dir, c.aTurn);
      c.aTurn = '';
    },

    async FinishB(c) {
      c.m.b = 'done';
      c.m.bSlot = false;
      drainNext(c);
      release(c.dir, c.bTurn);
      c.bTurn = '';
    },

    // A booting child dies before any history (the orb gave up, an init
    // threw): nothing on a page does that, so it is the SIGKILL.
    async KillA(c) {
      c.m.a = 'failed';
      c.m.aSlot = false;
      await kill(c);
    },

    async KillB(c) {
      c.m.b = 'failed';
      c.m.bSlot = false;
      await kill(c);
    },

    async StopA(c) {
      const was = c.m.a;
      c.m.a = was === 'queued' ? 'none' : 'stopped';
      c.m.aSlot = false;
      c.aTurn = ''; // the interrupt cancels a held model call
      await stopRow(c, A, c.a, was);
    },

    async StopB(c) {
      const was = c.m.b;
      c.m.b = was === 'queued' ? 'none' : 'stopped';
      c.m.bSlot = false;
      c.bTurn = '';
      await stopRow(c, B, c.b, was);
    },

    // serve goes down and comes back with no turn open (launchd). The
    // start runs the same link, so a launch fault is lifted only for the
    // exec of serve itself.
    async ServeRestart(c) {
      if (c.m.a === 'booting') { c.m.a = 'queued'; c.m.aSlot = false; }
      if (c.m.b === 'booting') { c.m.b = 'queued'; c.m.bSlot = false; }
      drainNext(c);
      c.restarting = true;
      await c.serve.shutdown();
      heldPids(c);
      const broken = c.m.fault === 'launch';
      if (broken) fs.renameSync(c.exe + '.gone', c.exe);
      try {
        await c.serve.resume(c.exe);
      } finally {
        if (broken) fs.renameSync(c.exe, c.exe + '.gone');
      }
      // The page reconnects on its own; its failed polls while serve was
      // down are this step's.
      await until('the page to reconnect', () => c.page.locator('header.thread-head h1').count(), (n) => n > 0);
    },
  },

  read: async (c) => {
    const s = await readUiState(c);
    c.restarting = false;
    return s;
  },
  // The Work dialog once there is work, the thread's heading before.
  status: (c) => c.page.locator('.work-popover, .work-sheet, header.thread-head h1').first(),
  sessions: (c) => (c.a ? [c.a] : []),
  expectedError: (c, text) => c.restarting && /ERR_CONNECTION_REFUSED|Failed to fetch/.test(text),
  async cleanup(c) {
    if (!fs.existsSync(c.exe) && fs.existsSync(c.exe + '.gone')) fs.renameSync(c.exe + '.gone', c.exe);
    for (const name of c.queued) {
      if (fs.existsSync(path.join(c.dir, name + '.taken'))) release(c.dir, name);
      else fs.rmSync(path.join(c.dir, name + '.json'), { force: true });
    }
    releaseStart(c);
  },
});
