// go/tests/model/specs/send_into_dying_child.fizz in the browser (recipe:
// go/tests/model/README.md): a prompt, an answer or a /model sent from the
// page to a session's child while it is exiting (the SIGINT tail) or has
// exited with serve still holding its lease (dead). The Go twin is
// go/tests/model/mbt/send_into_dying_child_test.go; this file drives the
// same steps, with Prompt, Retry, Edit, Resend, SetModel, Answer and Stop
// going through the page.
//
// Both windows last milliseconds in a real run, so serve runs with
// BOUGH_TEST_HOLD_DIR (internal/testhold), as in the Go adapter: every
// stdin line a child reads parks at <pid>.line.<n> until the walk writes
// <pid>.line.<n>.go (Take, ModelRead, Answer, and their tail twins), a
// child SIGINTed out of a turn parks at <pid>.tail after "cancelled"
// (removing it is Exit), and serve parks a child's exit goroutine at
// <pid>.drop before the lease is dropped (removing it is Drop). The files
// are named by pid, so one directory per worker serves every test in it.
//
// What is read off the DOM, and why the rest is not:
//
//   status    the sidebar row's label word (Waiting for you reads running,
//             as the spec's status does)
//   ask       the question card in the transcript
//   send      "failed": a "Not sent" row; "accepted": a prompt of ours
//             still on its way (a turn-sending section)
//   stopping  the composer's Stop button: "Stopping" | "Retry stop"
//   model     the picker names b, or it names the configured model (a);
//             with b named, a "Model changed" block in the transcript is
//             the recorded switch (b), and its absence is a line still
//             unread (queued | tail) or never read (lost), which the
//             parked lines say
//
//   child, sigint, lostAnswer, line, modelFirst: the supervisor's and the
//   pipe's, never on the page. child and sigint are this file's record of
//   the spec (each action checks the process got there); line comes off
//   the parked lines, as in the Go adapter; lostAnswer and modelFirst are
//   the spec's record of what the pipe holds.
//
// Where the page runs ahead of the spec:
//
//   - Settle is a React effect on `live`: the render that ends the last
//     live thing already cleared Stopping. While Settle is enabled in the
//     spec, the page must read "" and stopping is the spec's.
//   - Stop on a live child: the spec's SIGINT is "on its way" until
//     ChildSignal, so the page's POST /interrupt is answered 200 (what
//     serve answers) by the walk, and the same POST goes to serve at
//     ChildSignal. Holding the request instead would leave the page busy,
//     and a busy page disables the ask's options, so AnswerWhileSignalled
//     could never be pressed.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFileSync } from 'child_process';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release as releaseTurn, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { axeClean, uiInvariants } from '../../helpers/ui-invariants';
import { test, type Serve } from '../../helpers/serve';

// One per worker: its tests run one at a time, and every hold is named by
// the pid it parks.
const holdDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-sidc-hold-'));

// The picker lists what the catalogue lists; b is llm-control's, so the
// switch keeps the session on the model that is steered here. The page
// sends the provider with it, and serve writes this line.
const MODEL = 'b';
const MODEL_LINE = `/model llm-control ${MODEL}`;
const CATALOGUE = { providers: [{ plugin: 'llm-control', models: [{ id: MODEL }] }], efforts: [] as string[] };

const bound = 10_000;

interface Shadow {
  child: string; status: string; sigint: boolean; ask: boolean; lostAnswer: boolean;
  send: string; line: string; model: string; stopping: string; modelFirst: boolean;
}

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;          // llm-control's queue
  nonce: string;        // makes this test's lines unique in the shared hold dir
  n: number;
  sh: Shadow;
  pid: number;          // the session's current child, 0 when it has none
  pids: number[];       // every child this walk had, for cleanup
  held: string;         // the llm-control turn the running turn holds
  next: string;         // the turn queued for the request after an answer
  prompt: string;       // the page's current prompt text
  options: string[];    // the armed ask's options
  lostText: string;     // an answer the SIGINT beat, parked for good
  fakeStop: boolean;    // the next POST /interrupt is answered here
  stopSeen: number;     // POST /interrupt the page made
}

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const modelSel = (c: Ctx) => c.page.locator('.composer-tools .sel').filter({ has: c.page.locator('button[aria-label^="Next turn model"]') });
const failedRow = (c: Ctx) => c.page.locator('.send-failed').filter({ has: c.page.locator('strong', { hasText: /^Not sent$/ }) });

const WORDS: Record<string, string> = {
  Idle: 'idle', Running: 'running', 'Waiting for you': 'running', Done: 'done', Stopped: 'stopped', Interrupted: 'interrupted',
};

const settleEnabled = (s: Shadow) => s.send !== 'accepted' && s.status !== 'running' && s.stopping !== '';

// ── the hold files ──────────────────────────────────────────────────

const exists = (p: string) => fs.existsSync(p);
const holdFile = (c: Ctx, name: string) => path.join(holdDir, `${c.pid}.${name}`);

interface Parked { pid: number; path: string; gone: boolean }

// The parked line holding text, preferring the current child's.
function parked(c: Ctx, text: string): Parked | undefined {
  let found: Parked | undefined;
  for (const f of fs.readdirSync(holdDir)) {
    if (!/^\d+\.line\.\d+$/.test(f)) continue;
    const p = path.join(holdDir, f);
    let body = '';
    try { body = fs.readFileSync(p, 'utf8'); } catch { continue; }
    if (body !== text) continue;
    const pid = Number(f.split('.')[0]);
    const x = { pid, path: p, gone: exists(p + '.go') };
    if (!found || pid === c.pid) found = x;
  }
  return found;
}

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = bound): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what} (${ms}ms)`);
    await new Promise((r) => setTimeout(r, 20));
  }
}

async function waitLine(c: Ctx, text: string): Promise<Parked> {
  let p: Parked | undefined;
  await until(`a child to read ${JSON.stringify(text)}`, () => !!(p = parked(c, text)));
  return p!;
}

const lineNo = (p: string) => Number(p.slice(p.lastIndexOf('.') + 1));

// Lets a parked line go. stdin is FIFO: an earlier line of the same child
// still parked means the walk asked for an order no pipe has. The answer
// the SIGINT beat is the exception: the child reads it after the cancel
// and drops it, which leaving it parked stands for.
function letGo(c: Ctx, text: string): void {
  const p = parked(c, text);
  if (!p || p.gone) throw new Error(`no parked line ${JSON.stringify(text)} to let go`);
  const n = lineNo(p.path);
  for (const f of fs.readdirSync(holdDir)) {
    if (!f.startsWith(`${p.pid}.line.`) || f.endsWith('.go')) continue;
    const m = path.join(holdDir, f);
    if (exists(m + '.go') || lineNo(m) >= n) continue;
    const body = fs.readFileSync(m, 'utf8');
    if (!c.lostText || body !== c.lostText) throw new Error(`letting ${JSON.stringify(text)} go ahead of ${JSON.stringify(body)}, earlier in the pipe`);
  }
  fs.writeFileSync(p.path + '.go', '');
}

// Where a written line is: "" once read (or never written), else queued,
// tail or lost by the state of the child it went to.
function where(c: Ctx, text: string): string {
  const p = text ? parked(c, text) : undefined;
  if (!p || p.gone) return '';
  if (p.pid !== c.pid) return 'lost';
  return c.sh.child === 'alive' ? 'queued' : c.sh.child === 'tail' ? 'tail' : 'lost';
}

// ── the model and the processes ─────────────────────────────────────

function childPID(c: Ctx): number {
  const deadline = Date.now() + bound;
  for (;;) {
    let out = '';
    try { out = execFileSync('pgrep', ['-P', String(c.serve.pid), '-f', '--', '--headless'], { encoding: 'utf8' }); } catch { /* none yet */ }
    const f = out.split(/\s+/).filter(Boolean);
    if (f.length === 1) return Number(f[0]);
    if (Date.now() > deadline) throw new Error(`serve ${c.serve.pid} has ${f.length} headless children, want 1`);
    execFileSync('sleep', ['0.02']);
  }
}

function historyKinds(c: Ctx): { kind: string; text?: string }[] {
  const file = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  if (!exists(file)) return [];
  return fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).map((l) => {
    const e = JSON.parse(l);
    return { kind: e.kind, text: e.data?.text };
  });
}

const count = (c: Ctx, kind: string) => historyKinds(c).filter((e) => e.kind === kind).length;

const turnName = (c: Ctx) => `t${String(++c.n).padStart(5, '0')}`;

// Takes back turns nobody will take and releases the taken ones, so a
// later request never takes a stale turn.
function dropQueue(c: Ctx): void {
  for (const f of fs.existsSync(c.dir) ? fs.readdirSync(c.dir) : []) {
    if (f.endsWith('.json')) fs.rmSync(path.join(c.dir, f), { force: true });
    else if (f.endsWith('.taken') && !exists(path.join(c.dir, f.replace(/\.taken$/, '.release')))) releaseTurn(c.dir, f.replace(/\.taken$/, ''));
  }
  c.held = ''; c.next = '';
}

// ── reading the page ────────────────────────────────────────────────

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page, sh } = c;
  const d = await page.evaluate(({ id, prompt }) => {
    const label = document.querySelector(`button.row[data-id="${id}"]`)?.getAttribute('aria-label') ?? '';
    const failed = [...document.querySelectorAll('.send-failed strong')].some((s) => s.textContent === 'Not sent');
    // Our prompt still on its way: a section the page marks as sending.
    // Once the turn runs, the page hosts it in the prompt's own section
    // (no longer turn-sending) until an event brings the recorded input.
    const pending = [...document.querySelectorAll('.transcript section.turn-sending')]
      .some((s) => (s.querySelector('.prompt-bubble')?.textContent ?? '').includes(prompt) && prompt !== '');
    const stopping = document.querySelector('button.composer-stop-retry') ? 'failed'
      : document.querySelector('button.composer-stop[aria-label="Stopping"]') ? 'stopping' : '';
    const pick = document.querySelector('.composer-tools button[aria-label^="Next turn model"]')?.getAttribute('aria-label') ?? '';
    const switched = [...document.querySelectorAll('.transcript .block-label')].some((l) => l.textContent === 'Model changed');
    return { label, failed, pending, stopping, pick: pick.slice(pick.indexOf(': ') + 2), switched, ask: !!document.querySelector('.transcript .ask') };
  }, { id: c.id, prompt: c.prompt });

  const status = WORDS[d.label.split(', ')[1]] ?? `unknown: ${d.label}`;
  const send = d.failed && d.pending ? 'both failed and pending' : d.failed ? 'failed' : d.pending ? 'accepted' : '';
  let model = 'a';
  if (d.pick === MODEL) {
    if (d.switched) model = 'b';
    else model = where(c, MODEL_LINE) || 'lost';
  } else if (d.switched) model = `switched but the picker names ${d.pick}`;
  let stopping = d.stopping;
  if (settleEnabled(sh)) stopping = d.stopping === '' ? sh.stopping : `page did not settle: ${d.stopping}`;
  return {
    child: sh.child,
    status,
    sigint: sh.sigint,
    ask: d.ask,
    lostAnswer: sh.lostAnswer,
    send,
    line: send === 'accepted' ? where(c, c.prompt) : '',
    model,
    stopping,
    modelFirst: sh.modelFirst && (model === 'queued' || model === 'tail'),
  };
}

// ── the spec's own bookkeeping (ensure, deliver) ─────────────────────

function ensure(c: Ctx): void {
  if (c.sh.child === 'none') {
    c.sh.child = 'alive';
    if (c.sh.status === 'interrupted') c.sh.status = 'stopped';
  }
}

// A line the page wrote: into a dead child it is a 500 and nothing is
// parked; otherwise the line parks in the child ensure handed back (a
// respawn when the lease was dropped).
async function arrived(c: Ctx, text: string, respawn: boolean): Promise<void> {
  const p = await waitLine(c, text);
  if (respawn) {
    c.pid = p.pid;
    c.pids.push(p.pid);
  } else if (p.pid !== c.pid) {
    throw new Error(`${JSON.stringify(text)} reached child ${p.pid}, the session's is ${c.pid}`);
  }
}

async function sendPrompt(c: Ctx, text: string, click: () => Promise<void>): Promise<void> {
  const sh = c.sh;
  const dead = sh.child === 'dead', respawn = sh.child === 'none';
  const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/prompt`) && r.request().method() === 'POST');
  await click();
  const code = (await res).status();
  c.prompt = text;
  ensure(c);
  if (dead) {
    if (code !== 500) throw new Error(`prompt into an exited child: ${code}, want 500`);
    sh.send = 'failed';
    return;
  }
  if (code !== 200) throw new Error(`prompt: ${code}, want 200`);
  await arrived(c, text, respawn);
  sh.send = 'accepted';
  sh.line = sh.child === 'alive' ? 'queued' : 'tail';
  if (sh.model === 'queued' || sh.model === 'tail') sh.modelFirst = true;
}

async function compose(c: Ctx, text: string): Promise<void> {
  await c.page.locator('#composer').fill(text);
  await c.page.getByRole('button', { name: 'Send', exact: true }).click();
}

// The live child takes the prompt line: the loop records it and the turn
// runs on a held model request.
async function take(c: Ctx): Promise<void> {
  c.held = turnName(c);
  queue(c.dir, c.held, { mode: 'block', text: `finished ${c.held}` });
  letGo(c, c.prompt);
  await waitTaken(c.dir, c.held);
  await until('the input to be recorded', () => historyKinds(c).some((e) => e.kind === 'input' && e.text === c.prompt));
  Object.assign(c.sh, { line: '', send: '', status: 'running', lostAnswer: false });
}

async function readModel(c: Ctx): Promise<void> {
  letGo(c, MODEL_LINE);
  await until('the /model line to be recorded', () => historyKinds(c).some((e) => e.kind === 'model'));
  Object.assign(c.sh, { model: 'b', modelFirst: false });
}

async function answerWith(c: Ctx): Promise<string> {
  const text = c.options[0];
  const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/answer`) && r.request().method() === 'POST');
  await c.page.locator('.transcript .ask').getByRole('button', { name: text, exact: true }).click();
  const code = (await res).status();
  if (code !== 200) throw new Error(`answer: ${code}, want 200`);
  await waitLine(c, text);
  return text;
}

modelTests<Ctx>({
  spec: 'send_into_dying_child',
  role: 'Session#0',
  config: CONTROL_CONFIG,
  env: { BOUGH_TEST_HOLD_DIR: holdDir },
  // Chromium logs every non-2xx answer: the 500 of a line written to an
  // exited child, and the 404 of a Stop on a session with no child.
  expectedErrors: [/status of (500|404)/],

  async init(page, serve) {
    // A walk is up to 51 steps.
    test.setTimeout(600_000);
    // A control that never shows fails its step, not the whole budget.
    page.setDefaultTimeout(15_000);
    const c: Ctx = {
      page, serve, id: await serve.newSession(), dir: controlDir(serve.home), nonce: Math.random().toString(36).slice(2, 8), n: 0,
      sh: { child: 'alive', status: 'idle', sigint: false, ask: false, lostAnswer: false, send: '', line: '', model: 'a', stopping: '', modelFirst: false },
      pid: 0, pids: [], held: '', next: '', prompt: '', options: [], lostText: '', fakeStop: false, stopSeen: 0,
    };
    await page.route('**/api/models', (route) => route.fulfill({ json: CATALOGUE }));
    await page.route(`**/api/sessions/${c.id}/interrupt`, (route: Route) => {
      c.stopSeen++;
      if (!c.fakeStop) return route.continue();
      c.fakeStop = false;
      return route.fulfill({ json: { ok: true } });
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await modelSel(c).locator('button.sel-btn').waitFor();
    c.pid = childPID(c);
    c.pids.push(c.pid);
    return c;
  },

  actions: {
    Prompt: (c) => { const t = `prompt ${c.nonce} ${++c.n}`; return sendPrompt(c, t, () => compose(c, t)); },

    Retry: (c) => sendPrompt(c, c.prompt, () => failedRow(c).getByRole('button', { name: 'Retry', exact: true }).click()),

    async Edit(c) {
      // Edit never lands on a newer draft: a stopped prompt the page put
      // back in the composer is cleared first, as a person would.
      await c.page.locator('#composer').fill('');
      await failedRow(c).getByRole('button', { name: /^Edit/ }).click();
      await until('the Not sent row to go', async () => (await failedRow(c).count()) === 0);
      c.sh.send = '';
      c.prompt = '';
    },

    // The lost prompt still reads Waiting, so this send is a steer to the
    // page; the dropped lease makes it a prompt to a respawned child.
    Resend: (c) => { const t = `resend ${c.nonce} ${++c.n}`; return sendPrompt(c, t, () => compose(c, t)); },

    async SetModel(c) {
      const sh = c.sh;
      const dead = sh.child === 'dead', respawn = sh.child === 'none';
      const sel = modelSel(c);
      const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/model`) && r.request().method() === 'POST');
      // A switch that failed (a 500 from an exited child) leaves the list
      // open with "Couldn't save · Retry": the same request again.
      const failed = sel.locator('button.sel-save-failed');
      if (await failed.isVisible()) await failed.click();
      else {
        await sel.locator('button.sel-btn').click();
        await sel.locator('input.sel-search').fill(MODEL);
        await sel.getByRole('option').filter({ has: c.page.locator('.sel-label', { hasText: new RegExp(`^${MODEL}$`) }) }).click();
      }
      const code = (await res).status();
      ensure(c);
      if (dead) {
        if (code !== 500) throw new Error(`/model into an exited child: ${code}, want 500`);
        sh.modelFirst = false;
        return;
      }
      if (code !== 200) throw new Error(`/model: ${code}, want 200`);
      await arrived(c, MODEL_LINE, respawn);
      sh.model = sh.child === 'alive' ? 'queued' : 'tail';
      sh.modelFirst = sh.line !== 'queued' && sh.line !== 'tail';
    },

    async Answer(c) {
      const text = await answerWith(c);
      letGo(c, text);
      await waitTaken(c.dir, c.next);
      c.held = c.next; c.next = '';
      c.sh.ask = false;
    },

    // The same click after Stop: the POST is a 200, the child reads the
    // line into its hold, and the cancel ChildSignal makes wins.
    async AnswerWhileSignalled(c) {
      c.lostText = await answerWith(c);
      Object.assign(c.sh, { ask: false, lostAnswer: true });
    },

    async Stop(c) {
      const sh = c.sh;
      const before = c.stopSeen;
      c.fakeStop = sh.child === 'alive';
      const res = sh.child === 'alive' ? undefined
        : c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/interrupt`));
      await c.page.locator('button.composer-stop, button.composer-stop-retry').click();
      await until('the page to POST /interrupt', () => c.stopSeen > before);
      sh.stopping = 'stopping';
      if (sh.child === 'alive') { sh.sigint = true; return; }
      const code = (await res!).status();
      if (sh.child === 'none') {
        if (code !== 404) throw new Error(`stop with no child: ${code}, want 404`);
        sh.stopping = 'failed';
      } else if (code !== 200) {
        throw new Error(`stop a ${sh.child} child: ${code}, want 200`);
      }
    },

    async Take(c) { await take(c); },
    async ModelRead(c) { await readModel(c); },

    // The held request comes back as an ask; the request after the answer
    // takes the turn queued here.
    async Ask(c) {
      c.next = turnName(c);
      queue(c.dir, c.next, { mode: 'block', text: `finished ${c.next}` });
      c.options = [`red-${c.n}`, `blue-${c.n}`];
      releaseWith(c.dir, c.held, { mode: 'call', tool: 'ask', args: { question: 'Which colour?', options: c.options } });
      c.held = '';
      await c.page.locator('.transcript .ask').waitFor({ timeout: bound });
      c.sh.ask = true;
    },

    async Finish(c) {
      const before = count(c, 'done');
      releaseTurn(c.dir, c.held);
      c.held = '';
      await until('the turn to finish', () => count(c, 'done') > before);
      c.sh.status = 'done';
    },

    // Stop's SIGINT reaching the child: the page's POST, which the walk
    // answered at Stop, goes to serve now. A running turn is cancelled and
    // the child parks in its tail; an idle child exits at once.
    async ChildSignal(c) {
      const sh = c.sh;
      const running = sh.status === 'running';
      const holds = running ? ['tail', 'drop'] : ['drop'];
      for (const h of holds) fs.writeFileSync(holdFile(c, h), '');
      const before = count(c, 'cancelled');
      const res = await c.serve.api.post(`/api/sessions/${c.id}/interrupt`);
      if (!res.ok()) throw new Error(`interrupt: ${res.status()} ${await res.text()}`);
      await until('the SIGINTed child to park', () => exists(holdFile(c, running ? 'tail.reached' : 'drop.reached')));
      sh.sigint = false;
      if (running) {
        dropQueue(c);
        await until('the stop to be recorded', () => count(c, 'cancelled') > before);
        Object.assign(sh, { ask: false, status: 'stopped', child: 'tail' });
        if (sh.model === 'queued') sh.model = 'tail';
      } else {
        sh.child = 'dead';
        if (sh.model === 'queued') sh.model = 'lost';
        sh.modelFirst = false;
      }
    },

    async TailTake(c) { await take(c); },
    async TailModelRead(c) { await readModel(c); },

    // The tail unmounts and exits; what it did not read dies with it.
    async Exit(c) {
      const sh = c.sh;
      fs.rmSync(holdFile(c, 'tail'), { force: true });
      await until('the tail to exit', () => exists(holdFile(c, 'drop.reached')));
      dropQueue(c);
      sh.child = 'dead';
      if (sh.status === 'running') sh.status = 'stopped';
      if (sh.line === 'tail') sh.line = 'lost';
      if (sh.model === 'tail') sh.model = 'lost';
      sh.modelFirst = false;
    },

    async Crash(c) {
      const sh = c.sh;
      fs.writeFileSync(holdFile(c, 'drop'), '');
      process.kill(c.pid, 'SIGKILL');
      await until('serve to see the crash', () => exists(holdFile(c, 'drop.reached')));
      dropQueue(c);
      Object.assign(sh, { child: 'dead', sigint: false, modelFirst: false });
      if (sh.line === 'queued') sh.line = 'lost';
      if (sh.model === 'queued') sh.model = 'lost';
    },

    // serve's exit goroutine runs on: the exit event, then drop.
    async Drop(c) {
      const sh = c.sh;
      fs.rmSync(holdFile(c, 'drop'), { force: true });
      await until('the lease to drop', async () => {
        const res = await c.serve.api.get(`/api/sessions/${c.id}`);
        return res.ok() && (await res.json()).session.live === false;
      });
      c.pid = 0;
      Object.assign(sh, { child: 'none', ask: false });
      if (sh.status === 'running') sh.status = 'interrupted';
    },

    // The page's own effect; the read after it checks it ran.
    async Settle(c) { c.sh.stopping = ''; },
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async invariants(c, where) {
    // The composer carries every state here: Not sent, Stopping, Retry
    // stop, the status line. Clipping and the focus ring (Retry and Edit
    // put focus in the composer, which shows it by its caret) from
    // uiInvariants; axe from axeClean, which waits out the hints' fade.
    await uiInvariants(c.page, { include: [], text: '.composer-status, .composer-stop-retry, .send-failed-line strong' }, where);
    await axeClean(c.page, '.composer-wrap', where);
  },
  async cleanup(c) {
    for (const f of fs.readdirSync(holdDir)) {
      if (c.pids.some((p) => f.startsWith(`${p}.`))) fs.rmSync(path.join(holdDir, f), { force: true });
    }
    for (const p of c.pids) { try { process.kill(p, 'SIGKILL'); } catch { /* gone */ } }
    dropQueue(c);
  },
});
