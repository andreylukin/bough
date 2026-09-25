// go/tests/model/specs/background_agents.fizz in the browser (recipe:
// go/tests/model/README.md): a parent session's background agents as its
// Work dialog shows them. Every generated path is walked against a real
// serve whose model is llm-control, and at each node readUiState must
// equal the spec's Parent state.
//
// Two pages share the context: `page` is the parent with its Work dialog
// open (statuses, rows, the running count, the reports in its
// transcript), `child` is the first agent's own session (its closed
// turns, its composer for Message, its own Work button for depth).
//
// The parent is a session with no process, as in the Go adapter: a live
// parent would take every report as a wake turn, and its model calls
// would race the children's for llm-control's one queue. Each path gets
// its own serve (modelTests' per-test fixture): serve's running cap and
// llm-control's queue are process-wide, so a walk sharing a serve would
// see the last walk's agents in both.
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { expect, type Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

const MAX_PER_SESSION = 2;
const MAX_RUNNING = 1;
// The first and second agent are told apart in the dialog by title, so
// the titler (whose control-model name is "control" for both) is off:
// a session keeps its opening prompt as its title.
const FIRST = 'first task';
const SECOND = 'second task';

interface Ctx {
  page: Page;
  child: Page | null;   // the first agent's session, once spawned
  childErrors: string[];
  serve: Serve;
  parent: string;
  first: string;
  second: string;
  // How the first agent's current turn was opened: "serve" (spawn) or
  // "msg" (a message). The page shows one running count per parent; this
  // says which slot kind the count's share for the first agent is.
  firstSlot: string;
  firstTurn: string;    // held llm-control turns, '' when none
  secondTurn: string;
  secondQueued: boolean;
  turn: number;
}

const dir = (c: Ctx) => controlDir(c.serve.home);

function nextTurn(c: Ctx): string {
  const name = `b${String(++c.turn).padStart(4, '0')}`;
  queue(dir(c), name, { mode: 'block', text: `finished ${name}` });
  return name;
}

// A parent with no process: its history file and meta line, the way a
// session from before a serve restart looks.
function writeParent(home: string, cwd: string): string {
  const id = crypto.randomUUID();
  const file = path.join(home, '.bough', 'history', `${id}.jsonl`);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify({ seq: 1, at: new Date().toISOString(), kind: 'meta', data: { cwd } }) + '\n');
  return id;
}

const dialog = (c: Ctx) => c.page.getByRole('dialog', { name: /^Work/ });
const workButton = (p: Page) => p.locator('button.work-summary');
// A Work row by its agent's title (the row head's accessible name).
const agentRow = (c: Ctx, title: string) => dialog(c).locator('.work-row').filter({ has: c.page.getByRole('button', { name: title, exact: true }) });

const LIFE: Record<string, string> = { running: 'running', queued: 'queued', finished: 'done', failed: 'error', stopped: 'stopped' };

// The Work dialog stays open between steps; a fold (Finished) is opened
// so every agent's row is on screen.
async function openDialog(c: Ctx): Promise<boolean> {
  if (!(await workButton(c.page).count())) return false;
  if (!(await dialog(c).isVisible())) await workButton(c.page).click();
  for (const fold of await dialog(c).locator('.work-group-toggle[aria-expanded="false"]').all()) await fold.click();
  return true;
}

async function lifeOf(c: Ctx, title: string): Promise<string> {
  const row = agentRow(c, title);
  if (!(await row.count())) return 'none';
  const life = (await row.first().getAttribute('data-life')) ?? '';
  return LIFE[life] ?? `unknown: ${life}`;
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  if (c.childErrors.length) throw new Error(`child page console errors: ${c.childErrors.join('\n')}`);
  const open = await openDialog(c);
  const first = open ? await lifeOf(c, FIRST) : 'none';
  const second = open ? await lifeOf(c, SECOND) : 'none';
  const total = open ? await dialog(c).locator('.work-row').count() : 0;
  // The Work button's "N running": the agents holding a slot, as the
  // parent row counts them. It is per parent, so it is attributed the way
  // the Go adapter does: open turns first, the second agent's before the
  // first's (its start is serve's own), and a count beyond them to a
  // closed agent that leaked its slot.
  const label = open ? (await workButton(c.page).getAttribute('aria-label')) ?? '' : '';
  let left = Number(/(\d+) running/.exec(label)?.[1] ?? 0);
  const holds = (yes: boolean) => (yes && left > 0 ? (left--, true) : false);
  let secondHolds = holds(second === 'running');
  let firstHolds = holds(first === 'running');
  if (!firstHolds && first !== 'none') firstHolds = holds(true);
  if (!secondHolds && second !== 'none' && second !== 'queued') secondHolds = holds(true);
  // The parent's transcript shows one report row per closed turn of an
  // agent, named by its title.
  const firstNotices = await c.page.locator('.agent-notice .agent-notice-name').evaluateAll(
    (els, t) => els.filter((e) => e.getAttribute('title') === t).length, FIRST);
  // The first agent's own transcript: one footer per closed turn (a
  // failed turn's footer says so in its error, not as an outcome word).
  const firstTurns = c.child ? await c.child.locator('section.turn .turn-foot').count() : 0;
  // A background agent that had started one would have work of its own.
  const grand = c.child ? await workButton(c.child).count() : 0;
  return {
    first, first_slot: firstHolds ? c.firstSlot : '', first_turns: firstTurns, first_notices: firstNotices,
    second, second_slot: secondHolds ? 'serve' : '', total, grand,
  };
}

// tools.spawn({background}) is the agent's call, not the person's: it
// has no UI, so it is the POST the tool makes.
async function spawn(c: Ctx, parent: string, prompt: string): Promise<{ status: number; id: string; queued: boolean }> {
  const res = await c.serve.api.post('/api/sessions', {
    data: { cwd: c.serve.work, prompt, spawnedBy: parent, maxPerSession: MAX_PER_SESSION, maxRunning: MAX_RUNNING },
  });
  if (!res.ok()) return { status: res.status(), id: '', queued: false };
  const body = await res.json();
  return { status: res.status(), id: body.session.id, queued: !!body.queued };
}

async function childLive(c: Ctx, id: string): Promise<boolean> {
  const res = await c.serve.api.get(`/api/sessions/${c.parent}/children`);
  const rows = (await res.json()).children as { id: string; live: boolean }[];
  return rows.some((r) => r.id === id && r.live);
}

// The close that frees the running slot drains the queued agent at once,
// so its model turn is queued before the close.
function beforeClose(c: Ctx): void {
  if (c.secondQueued) {
    c.secondTurn = nextTurn(c);
    c.secondQueued = false;
  }
}

async function afterClose(c: Ctx): Promise<void> {
  if (c.secondTurn && !c.secondQueued) await waitTaken(dir(c), c.secondTurn);
}

// A Stop waits out serve's 20 s interrupt hold (see Stop), past the
// default 30 s a test gets once the rest of its path is added.
test.describe.configure({ timeout: 90_000 });

modelTests<Ctx>({
  spec: 'background_agents',
  role: 'Parent#0',
  config: CONTROL_CONFIG + '- id: session-title\n  plugin: session-title\n  disabled: true\n',
  shared: true,
  reset: true,

  async init(page, serve) {
    const c: Ctx = {
      page, child: null, childErrors: [], serve, parent: writeParent(serve.home, serve.work), first: '', second: '',
      firstSlot: '', firstTurn: '', secondTurn: '', secondQueued: false, turn: 0,
    };
    await page.goto(`${serve.url}/#/s/${c.parent}`);
    return c;
  },

  actions: {
    async Spawn(c) {
      if (c.first && c.second) {
        const r = await spawn(c, c.parent, 'third task');
        expect(r.status, 'a spawn past the budget').toBe(429);
        return;
      }
      if (!c.first) {
        const name = nextTurn(c);
        const r = await spawn(c, c.parent, FIRST);
        expect(r.status, 'first spawn').toBe(201);
        expect(r.queued, 'first spawn queued').toBe(false);
        c.first = r.id;
        c.firstSlot = 'serve';
        c.firstTurn = name;
        await waitTaken(dir(c), name);
        c.child = await c.page.context().newPage();
        c.child.on('console', (m) => { if (m.type() === 'error') c.childErrors.push(m.text()); });
        c.child.on('pageerror', (e) => c.childErrors.push(String(e)));
        await c.child.goto(`${c.serve.url}/#/s/${c.first}`);
        // A hidden page does not poll its list; the person is on the parent.
        await c.page.bringToFront();
        return;
      }
      const r = await spawn(c, c.parent, SECOND);
      expect(r.status, 'second spawn').toBe(201);
      expect(r.queued, 'second spawn queued').toBe(true);
      c.second = r.id;
      c.secondQueued = true;
    },

    // The running agent calling tools.spawn itself: refused at depth 1.
    async AgentSpawns(c) {
      const r = await spawn(c, c.first, 'grandchild task');
      expect(r.status, 'a background agent spawning').toBe(409);
    },

    async Finish(c) {
      beforeClose(c);
      release(dir(c), c.firstTurn);
      c.firstTurn = '';
      await afterClose(c);
    },

    async Fail(c) {
      beforeClose(c);
      releaseWith(dir(c), c.firstTurn, { mode: 'error', error: 'model says no' });
      c.firstTurn = '';
      await afterClose(c);
    },

    // Stop agent in the Work dialog; the interrupt cancels the held call.
    //
    // serve holds an interrupt while the prompt it wrote is "unread", and
    // the headless child never says it read one (no "input" line): the
    // hold ends at the child's first output or after holdLimit (20 s).
    // llm-control's held turn prints nothing, so the stop lands only at
    // holdLimit, past the button's own 15 s patience ("No reply"). The
    // wait is on the API, with the page's clock still, so the page is
    // read once the stop has landed, as the Go adapter does. See the
    // flow's notes: this is the known "input" finding, not fixed here.
    async Stop(c) {
      beforeClose(c);
      await openDialog(c);
      await agentRow(c, FIRST).getByRole('button', { name: 'Stop agent' }).click();
      c.firstTurn = '';
      await expect.poll(async () => {
        const res = await c.serve.api.get(`/api/sessions/${c.parent}/children`);
        return ((await res.json()).children as { id: string; status: string }[]).find((r) => r.id === c.first)?.status;
      }, { message: 'the stop to land', timeout: 30_000 }).toBe('stopped');
      await afterClose(c);
    },

    async FinishSecond(c) {
      release(dir(c), c.secondTurn);
      c.secondTurn = '';
    },

    // Stop on the queued agent's row in the Work dialog.
    async StopQueued(c) {
      await openDialog(c);
      await agentRow(c, SECOND).getByRole('button', { name: 'Stop agent' }).click();
      c.secondQueued = false;
    },

    // A person messages the finished agent from its own page.
    async Message(c) {
      const name = nextTurn(c);
      const child = c.child!;
      await child.getByRole('textbox', { name: 'Message this background agent' }).fill(`task ${name}`);
      await child.getByRole('button', { name: 'Send', exact: true }).click();
      c.firstTurn = name;
      c.firstSlot = 'msg';
      await waitTaken(dir(c), name);
    },

    // The idle agent's process ends (killed, or serve restarted under
    // it). Nothing on the page ends a process, so it is the interrupt
    // another client sends; an idle headless child exits on it.
    async Exit(c) {
      const res = await c.serve.api.post(`/api/sessions/${c.first}/interrupt`);
      if (!res.ok() && res.status() !== 404) throw new Error(`interrupt: ${res.status()} ${await res.text()}`);
      // After a failed turn the child printed only an error, which does
      // not end serve's interrupt hold (see Stop): this waits out holdLimit.
      await expect.poll(() => childLive(c, c.first), { message: 'the first agent to exit', timeout: 30_000 }).toBe(false);
    },
  },

  read: readUiState,
  // The Work dialog once there is work, the thread's heading before.
  status: (c) => c.page.locator('.work-popover, .work-sheet, header.thread-head h1').first(),
  sessions: (c) => (c.first ? [c.first] : []),
  async cleanup(c) {
    for (const name of [c.firstTurn, c.secondTurn]) if (name) release(dir(c), name);
  },
});
