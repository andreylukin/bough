// go/tests/model/specs/ask_answer_arm_races.fizz in the browser (recipe:
// go/tests/model/README.md): the model asks a question (A, then the
// secret B), serve arms it only when its stdout pump reads the child's
// event, two tabs answer the same question, and a message can land in
// the gap. The Go twin is go/tests/model/mbt/ask_answer_arm_races_test.go,
// and this file drives the same serve hooks: the pipes between serve and
// the child are stepped one line at a time through internal/linegate
// (BOUGH_TEST_LINE_GATE, a folder in each walk's own cwd), the model is
// llm-control, a timeout fires on cue through BOUGH_TEST_ASK_EXPIRE_DIR,
// and the secret lands in a file keychain.
//
// Three pages: tab0 and tab1 are the two answerers, each with its POST
// held by a route until the spec delivers it, and the harness's own page
// is a watcher reloaded before every read. A live page's transcript
// follows serve's event stream, which stops at a held stdout line, so
// what history already says (the question, how many were asked, where a
// line landed) is what a fresh load of the thread shows.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

const PROJECT = 'p1';
const SECRET = 'TOKEN';
const GATE = '.gate'; // relative: each child gates in its own cwd

// One per worker (the serve is too): expire files are named by ask id,
// which is never reused, and walks in a worker run one at a time.
const expireDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-aaar-expire-'));
const keychainDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-aaar-keychain-'));
const keychainFile = path.join(keychainDir, `bough%${PROJECT}%${SECRET}`);

// Turn names and walk folders are unique per serve, which is per worker.
let turns = 0;
let walks = 0;

type Q = 'A' | 'B';

interface Held { route: Route; tab: Page; q: string; text: string }

interface Ctx {
  page: Page;                // the watcher
  tab0: Page;
  tab1: Page;
  serve: Serve;
  id: string;
  cwd: string;
  dir: string;               // llm-control's queue
  held: string;              // the model request held in flight
  next: string;              // the block turn queued for the next request
  n: number;                 // answer and message markers
  errors: string[];          // console errors on the two tabs
  // The spec's own: what each tab has in flight, the one message, and
  // every line serve accepted for the child's stdin ("start" first).
  t0: Held | null;
  t1: Held | null;
  // The question each tab was answering when it sent: the one on its screen.
  shown: { t0: string; t1: string };
  prompt: { route: Route | null; tab: Page | null; text: string } | null;
  prompted: boolean;
  lines: string[];
  wrote: string[];
  dupWrite: boolean;
  unarmed: boolean;
  wrongDA: boolean;
}

// ── serve's side, which no page shows ───────────────────────────────

interface Entry { kind: string; text?: string; data?: Record<string, unknown> }

function entries(c: Ctx): Entry[] {
  const file = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));
}

// What history says the child has open, and the ask and ask-end lines
// it printed for serve's pump, in order (the Go adapter's view).
function view(c: Ctx) {
  let asked = 0, open = '', openID = '';
  const lines: string[] = [];
  for (const e of entries(c)) {
    const d = e.data ?? {};
    if (e.kind === 'ask') {
      asked++;
      open = d.secret ? 'B' : 'A';
      openID = String(d.id);
      lines.push(open);
    } else if (e.kind === 'ask/answer' && String(d.id) === openID) {
      open = ''; openID = '';
    } else if (e.kind === 'call' && (d.tool === 'ask' || d.tool === 'secret') && d.phase !== 'start') {
      const q = d.tool === 'secret' ? 'B' : 'A';
      lines.push('end' + q);
      if (open === q) { open = ''; openID = ''; }
    }
  }
  const n = passed(c, 'out');
  return { asked, open, openID, lines, out: lines.slice(n), read: lines.slice(0, n) };
}

const gateFile = (c: Ctx, side: string, n: number, ext: string) => path.join(c.cwd, GATE, `${side}.${n}.${ext}`);

function passed(c: Ctx, side: string): number {
  let n = 0;
  while (fs.existsSync(gateFile(c, side, n, 'done'))) n++;
  return n;
}

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}

// let lets line n of side through and returns what it became.
async function letLine(c: Ctx, side: string, n: number): Promise<string> {
  await until(`${side} line ${n} to arrive`, () => fs.existsSync(gateFile(c, side, n, 'held')));
  fs.writeFileSync(gateFile(c, side, n, 'go'), '');
  await until(`${side} line ${n} to be handled`, () => fs.existsSync(gateFile(c, side, n, 'done')));
  return fs.readFileSync(gateFile(c, side, n, 'done'), 'utf8');
}

// serve's arm, probed as another client would without touching it: an
// answer naming no real question is refused as expired while one is
// armed, and as no pending ask otherwise. Asks never overlap, so the
// armed one is the last one serve's pump read.
async function armed(c: Ctx): Promise<string> {
  const res = await c.serve.api.post(`/api/sessions/${c.id}/answer`, { data: { text: '', ask: 'probe-not-an-ask' } });
  const msg = await res.text();
  if (res.status() === 409 && msg.includes('no pending ask')) return '';
  if (res.status() !== 409 || !msg.includes('expired')) throw new Error(`arm probe: ${res.status()} ${msg}`);
  const read = view(c).read.filter((l) => l === 'A' || l === 'B');
  return read.at(-1) ?? 'armed before any ask was read';
}

// ── the model ───────────────────────────────────────────────────────

function queueNext(c: Ctx): void {
  c.next = `aaar${String(++turns).padStart(5, '0')}`;
  queue(c.dir, c.next, { mode: 'block', text: `finished ${c.next}` });
}

async function takeNext(c: Ctx): Promise<void> {
  await waitTaken(c.dir, c.next);
  c.held = c.next;
  queueNext(c);
}

async function rowStatus(c: Ctx): Promise<string> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  return (await res.json()).session.status;
}

// The turn ends; a steer that landed meanwhile makes one more request,
// which is let go too.
async function finishTurn(c: Ctx): Promise<void> {
  for (;;) {
    if (!c.held) throw new Error('finish: no model request is held');
    release(c.dir, c.held);
    c.held = '';
    let done = false;
    await until('the turn to end or ask again', async () => {
      if (fs.existsSync(path.join(c.dir, c.next + '.taken'))) return true;
      done = (await rowStatus(c)) === 'done';
      return done;
    });
    if (done) return;
    c.held = c.next;
    queueNext(c);
  }
}

async function askTool(c: Ctx, tool: 'ask' | 'secret', args: Record<string, unknown>, asked: number): Promise<void> {
  releaseWith(c.dir, c.held, { mode: 'call', tool, args });
  c.held = '';
  await until('the ask', () => { const v = view(c); return v.asked === asked && v.open !== ''; });
}

// The open question's call has ended in history: one more end line than
// before. The question closes earlier, on its answer entry.
function ended(c: Ctx, before: ReturnType<typeof view>): () => boolean {
  const n = before.lines.length;
  return () => { const v = view(c); return v.open === '' && v.lines.length > n; };
}

// ── the pages ───────────────────────────────────────────────────────

const row = (p: Page, c: Ctx) => p.locator(`button.row[data-id="${c.id}"]`).first();

const WORDS: Record<string, string> = { Running: 'running', 'Waiting for you': 'running', Done: 'done' };

// One synchronous read of the watcher.
async function dom(c: Ctx) {
  return c.page.evaluate((id) => {
    const row = document.querySelector(`button.row[data-id="${id}"]`);
    const card = document.querySelector('.transcript .ask');
    const t = document.querySelector<HTMLElement>('.transcript');
    return {
      label: row?.getAttribute('aria-label') ?? '',
      card: card ? (card.querySelector('input[aria-label="Secret value"]') ? 'B' : 'A') : '',
      // An ask or secret call that ended (answered or timed out) is its
      // recorded block. The open one is its card: its running block is
      // live only, on screen once serve's pump has read the call's start.
      ended: [...document.querySelectorAll('.transcript .call-native[data-seq] .block-label')].filter((l) => /^(Ask|Secret)$/.test(l.textContent?.trim() ?? '')).length,
      meta: [...document.querySelectorAll<HTMLElement>('.transcript .meta-line')].map((m) => m.innerText.trim()),
      bubbles: [...document.querySelectorAll<HTMLElement>('.transcript .prompt-bubble')].map((b) => b.innerText.trim()),
      text: t?.innerText ?? '',
    };
  }, c.id);
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  // A fresh load: what history says now, not what the stream has sent.
  await c.page.reload();
  await row(c.page, c).waitFor();
  await c.page.locator('.transcript').waitFor();
  const d = await dom(c);
  const status = WORDS[d.label.split(', ')[1]] ?? `unknown: ${d.label}`;

  // What the transcript betrays: a message given to a question, an
  // answer read as a message or steer (any prompt past the first), the
  // secret's text anywhere.
  const answers = (s: string) => s === 'red' || s === 'blue' || s.startsWith('S3CR3T-');
  let eaten = d.meta.some((m) => /^You answered: msg-/.test(m));
  const asPrompt = d.bubbles.slice(1).some(answers);
  const leak = d.text.includes('S3CR3T-');
  // Not on the page: the keychain holds a message given to the secret.
  try { if (fs.readFileSync(keychainFile, 'utf8').startsWith('msg-')) eaten = true; } catch { /* none stored */ }

  // Not on the page either: serve's arm (the row's "needs you" is
  // history's question, which is `open`), the child's stdout lines
  // serve has not read, and the stdin lines the child has not routed.
  const v = view(c);
  const inN = passed(c, 'in');
  return {
    status,
    asked: d.ended + (d.card ? 1 : 0),
    open: d.card,
    out: v.out,
    armed: await armed(c),
    pipe: c.lines.slice(inN),
    tab0: c.t0?.q ?? '',
    tab1: c.t1?.q ?? '',
    prompt: c.prompt !== null,
    prompted: c.prompted,
    wrote: [...c.wrote],
    dupWrite: c.dupWrite,
    unarmedWrite: c.unarmed,
    wrongDisarm: c.wrongDA,
    promptEaten: eaten,
    answerAsPrompt: asPrompt,
    secretLeak: leak,
  };
}

// The question on a tab's screen: its list poll brings it (the harness
// runs the page clock between reads; this moves it until it lands).
async function showing(c: Ctx, tab: Page, q: Q): Promise<void> {
  const want = q === 'B' ? tab.getByLabel('Secret value') : tab.locator('.transcript .ask .ask-option').first();
  for (let i = 0; i < 20 && !(await want.isVisible()); i++) {
    await tab.clock.fastForward(5_000);
    await new Promise((r) => setTimeout(r, 50));
  }
  await want.waitFor({ timeout: 5_000 });
}

// A tab answers the question it shows; its POST is held until delivered.
async function click(c: Ctx, tab: Page, which: 't0' | 't1', q: Q, option: string): Promise<void> {
  await showing(c, tab, q);
  c.shown[which] = q;
  let text = option;
  if (q === 'B') {
    text = `S3CR3T-${++c.n}`;
    await tab.getByLabel('Secret value').fill(text);
    await tab.getByLabel('Secret value').press('Enter');
  } else {
    await tab.locator('.transcript .ask .ask-option', { hasText: option }).click();
  }
  await until(`${which}'s answer to be sent`, () => c[which] !== null && c[which]!.text === text);
}

async function deliver(c: Ctx, which: 't0' | 't1'): Promise<void> {
  const h = c[which]!;
  const before = await armed(c);
  const res = h.tab.waitForResponse((r) => r.request() === h.route.request());
  await h.route.continue();
  const status = (await res).status();
  c[which] = null;
  if (status === 409) return; // refused: nothing written
  if (status !== 200) throw new Error(`answer ${h.q}: ${status}`);
  if (before !== h.q) c.unarmed = true;
  if (c.wrote.includes(h.q)) c.dupWrite = true;
  c.wrote.push(h.q);
  c.lines.push('ans' + h.q);
  if (before !== h.q && before !== '' && (await armed(c)) === '') c.wrongDA = true;
}

async function hold(c: Ctx, tab: Page, which: 't0' | 't1'): Promise<void> {
  await tab.route(new RegExp(`/api/sessions/${c.id}/(answer|prompt)$`), (route) => {
    const body = JSON.parse(route.request().postData() ?? '{}');
    if (route.request().url().endsWith('/prompt')) {
      c.prompt = { route, tab, text: body.text };
      return;
    }
    c[which] = { route, tab, q: c.shown[which], text: body.text };
  });
}

// ── the flow ────────────────────────────────────────────────────────

modelTests<Ctx>({
  spec: 'ask_answer_arm_races',
  role: 'Session#0',
  config: CONTROL_CONFIG,
  env: { BOUGH_TEST_ASK_EXPIRE_DIR: expireDir, BOUGH_TEST_KEYCHAIN_DIR: keychainDir, BOUGH_TEST_LINE_GATE: GATE },
  shared: true,

  async init(page, serve) {
    // Three pages and a reload per read.
    test.setTimeout(90_000);
    fs.rmSync(keychainFile, { force: true });
    // A project is a directory: the secret is stored for this one.
    const proj = path.join(serve.home, '.bough', 'projects', PROJECT);
    fs.mkdirSync(proj, { recursive: true });
    fs.writeFileSync(path.join(proj, 'project.yml'), `name: ${PROJECT}\n`);

    const cwd = path.join(serve.work, `aaar${++walks}`);
    fs.mkdirSync(cwd, { recursive: true });
    const c: Ctx = {
      page, tab0: page, tab1: page, serve, id: '', cwd, dir: controlDir(serve.home), held: '', next: '', n: 0, errors: [],
      t0: null, t1: null, shown: { t0: '', t1: '' }, prompt: null, prompted: false, lines: ['start'], wrote: [], dupWrite: false, unarmed: false, wrongDA: false,
    };
    queueNext(c);
    const res = await serve.api.post('/api/sessions', { data: { cwd, prompt: `start ${c.next}` } });
    if (!res.ok()) throw new Error(`create: ${res.status()} ${await res.text()}`);
    c.id = (await res.json()).session.id;
    await letLine(c, 'in', 0);
    await takeNext(c);
    await until('the turn to run', async () => (await rowStatus(c)) === 'running');

    c.tab0 = await page.context().newPage();
    c.tab1 = await page.context().newPage();
    for (const [tab, which] of [[c.tab0, 't0'], [c.tab1, 't1']] as const) {
      // A refusal the spec models (409) is logged by Chromium, and the
      // page says so beside the question.
      tab.on('console', (m) => { if (m.type() === 'error' && !/status of 409 /.test(m.text())) c.errors.push(`${which}: ${m.text()}`); });
      tab.on('pageerror', (e) => c.errors.push(`${which}: ${e}`));
      await hold(c, tab, which);
      await tab.goto(`${serve.url}/#/s/${c.id}`);
      await row(tab, c).waitFor();
    }
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await row(page, c).waitFor();
    return c;
  },

  actions: {
    async AskA(c) {
      await askTool(c, 'ask', { question: 'Which colour?', options: ['red', 'blue'] }, 1);
      // The child routes stdin by the question it has open from the
      // moment it prints the event: wait for serve's pump to hold it.
      await until("A's event on stdout", () => fs.existsSync(gateFile(c, 'out', 0, 'held')));
    },
    AskB: (c) => askTool(c, 'secret', { name: SECRET, question: 'the API token', project: PROJECT }, 2),

    async Timeout(c) {
      const v = view(c);
      fs.writeFileSync(path.join(expireDir, v.openID), '');
      await until("the ask's call to end", ended(c, v));
      await takeNext(c);
    },

    Finish: finishTurn,

    async ArmFromEvent(c) {
      await letLine(c, 'out', passed(c, 'out'));
    },

    // The child routes its next stdin line; then what that route does:
    // an answer returns the ask (its call ends, the model is asked
    // again), a steer lands in history, a message with no turn running
    // is a turn of its own, which is let finish.
    async ReadLine(c) {
      const v = view(c);
      const before = entries(c).length;
      const status = await rowStatus(c);
      const n = passed(c, 'in');
      const route = await letLine(c, 'in', n);
      switch (route) {
        case 'answer':
          await until("the answered ask's call to end", ended(c, v));
          return takeNext(c);
        case 'steer':
          return until('the steer', () => entries(c).slice(before).some((e) => e.kind === 'input'));
        case 'input':
          if (status !== 'done') return; // queued behind the running turn: Finish runs it
          await takeNext(c);
          return finishTurn(c);
        case 'drop':
          return;
      }
      throw new Error(`stdin line ${n} went to ${JSON.stringify(route)}`);
    },

    ClickTab0: (c) => click(c, c.tab0, 't0', view(c).open as Q, 'red'),
    // The second answerer of A: another tab showing the same question.
    ClickTab1: (c) => click(c, c.tab1, 't1', 'A', 'blue'),
    DeliverTab0: (c) => deliver(c, 't0'),
    DeliverTab1: (c) => deliver(c, 't1'),

    // A message: typed into a tab's composer and sent as a steer, from a
    // tab with no answer of its own in flight (its composer waits for
    // that). A page showing a question offers no way to send one (a
    // draft started there is its answer), so when neither tab can, the
    // message comes from another client, as the CLI's `bough send` or a
    // tab that polled late would send it.
    async SendPrompt(c) {
      c.prompted = true;
      const text = `msg-${++c.n}`;
      let tab: Page | null = null;
      for (const [t, h] of [[c.tab1, c.t1], [c.tab0, c.t0]] as const) {
        if (!h && !(await t.locator('.transcript .ask').count())) { tab = t; break; }
      }
      if (!tab) {
        c.prompt = { route: null, tab: null, text };
        return;
      }
      await tab.locator('#composer').fill(text);
      await tab.locator('.composer .btn-primary').click();
      await until('the message to be sent', () => c.prompt !== null);
    },

    // api.go prompt: 409 while an ask is armed, else the line is written.
    async DeliverPrompt(c) {
      const p = c.prompt!;
      let status: number;
      if (p.route) {
        const res = p.tab!.waitForResponse((r) => r.request() === p.route!.request());
        await p.route.continue();
        status = (await res).status();
      } else {
        status = (await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: p.text } })).status();
      }
      c.prompt = null;
      if (status === 409) return;
      if (status !== 200) throw new Error(`prompt: ${status}`);
      c.lines.push('prompt');
    },
  },

  read: readUiState,
  status: (c) => row(c.page, c),
  sessions: (c) => [c.id],
  async invariants(c, where) {
    if (c.errors.length) throw new Error(`${where}: console errors on the tabs: ${c.errors.join('\n')}`);
  },

  // Let the child's lines drain, take back what it had queued, and stop
  // it: the serve is the worker's, and the next walk starts clean.
  async cleanup(c) {
    for (const h of [c.t0, c.t1]) await h?.route.abort().catch(() => {});
    await c.prompt?.route?.abort().catch(() => {});
    fs.mkdirSync(path.join(c.cwd, GATE), { recursive: true });
    fs.writeFileSync(path.join(c.cwd, GATE, 'open'), '');
    if (c.held) release(c.dir, c.held);
    c.held = '';
    if (c.next) fs.rmSync(path.join(c.dir, c.next + '.json'), { force: true });
    for (const f of fs.readdirSync(expireDir)) fs.rmSync(path.join(expireDir, f), { force: true });
    await c.tab0.close().catch(() => {});
    await c.tab1.close().catch(() => {});
    const work = fs.realpathSync(c.cwd);
    let out = '';
    try { out = execFileSync('lsof', ['-a', '-d', 'cwd', '-c', 'bough', '-Fpn'], { encoding: 'utf8' }); } catch { /* none */ }
    let pid = 0;
    for (const l of out.split('\n')) {
      if (l.startsWith('p')) pid = Number(l.slice(1));
      else if (l.startsWith('n') && l.slice(1) === work && pid) try { process.kill(pid, 'SIGKILL'); } catch { /* gone */ }
    }
  },
});
