// go/tests/model/specs/ask_beside_parallel_calls.fizz in the browser
// (recipe: go/tests/model/README.md): the engine's one reply is a native
// ask plus a run_js or a bash call, run in parallel, and the sibling's
// end, a stderr line, a refusal note and a provider error land while the
// question is on screen. The person answers, sends and stops from the
// page. The Go twin, which says how each model-side event is made real,
// is go/tests/model/mbt/ask_beside_parallel_calls_test.go.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken, type Turn } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

// One per worker: tests in a worker run one at a time, and each walk's
// cleanup empties it.
const expireDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-abpc-expire-'));

const PERR = 'abpc provider boom';
const REFUSAL = 'abpc refusal note';
// Outlasts the harness's one-second grace for a reply's calls
// (coordinator toolCallRunGracePeriod): the spec's reply includes it.
const GRACE_MS = 1200;
// The sibling's command waits on this; every sibling block shows it.
const WAIT = 'while [ ! -s';

interface Call { id: string; name: string; args: Record<string, unknown> }
// llm-control's release with several calls, or a refusal (the Go
// twin's control.Turn has the same fields).
type Release = Turn | { mode: 'refuse'; error: string } | { mode: 'block'; calls: Call[] };

type Asker = 'none' | 'pending' | 'open' | 'job' | 'answered' | 'timedout' | 'cancelled';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;        // llm-control's queue
  turn: number;
  n: number;          // message and stderr markers
  next: string;       // the block turn queued for the next request
  held: string[];     // requests the engine made and the test holds, oldest first
  sibFile: string;    // the sibling ends when this says "ok" or "fail"
  sibSaid: string;
  // What the test's llm saw: a call ended, or a steer landed, while a
  // request was held, and no request came of it (the engine keeps it
  // for that request's answer). Cleared when the held request answers.
  unsent: boolean;
  queued: boolean;
  // The ask on the page when the message was sent, and when it timed out.
  askerAtSend: Asker | '';
  lost: boolean;
}

interface Entry { kind: string; text?: string; data?: Record<string, unknown> }

// ── the transcript file: driving only (when the engine's event landed,
// and the ask's id to time it out). The state is read off the page. ──

function entries(c: Ctx): Entry[] {
  const file = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));
}

const said = (e: Entry) => String(e.text ?? '') + ' ' + String(e.data?.text ?? '');

async function until(what: string, ok: () => Promise<boolean> | boolean, ms = 20_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 20));
  }
}

async function waitEntry(c: Ctx, from: number, what: string, ok: (e: Entry) => boolean): Promise<void> {
  await until(what, () => {
    absorb(c);
    return entries(c).slice(from).some(ok);
  });
}

async function sessionRow(c: Ctx): Promise<{ status: string; live: boolean }> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  return (await res.json()).session;
}

// ── driving the model ───────────────────────────────────────────────

function queueNext(c: Ctx): void {
  c.next = `t${String(++c.turn).padStart(5, '0')}`;
  queue(c.dir, c.next, { mode: 'block', text: `finished ${c.next}` });
}

// A request the engine made: hold it, and queue one for the next.
function absorb(c: Ctx): boolean {
  if (!fs.existsSync(path.join(c.dir, c.next + '.taken'))) return false;
  c.held.push(c.next);
  queueNext(c);
  return true;
}

// Answer the newest held request (with its own text when turn is
// absent). What waited for it goes out now.
function answer(c: Ctx, turn?: Release): void {
  const name = c.held.pop();
  if (!name) throw new Error('no model request is held');
  if (turn) releaseWith(c.dir, name, turn as Turn);
  else release(c.dir, name);
  c.unsent = c.queued = false;
}

// Wait until the transcript, the row and the llm queue have been still
// for a moment, holding any request the engine made meanwhile: one step
// sets off several asynchronous events (a call's end, a new request, a
// steer landing), and the state is compared after all of them.
async function settle(c: Ctx): Promise<void> {
  let last = '', lastAt = Date.now();
  await until('the session to settle', async () => {
    if (absorb(c)) lastAt = Date.now();
    const row = await sessionRow(c);
    const sig = `${row.status} ${row.live} ${entries(c).length} ${c.held.length}`;
    if (sig !== last) { last = sig; lastAt = Date.now(); return false; }
    return Date.now() - lastAt >= 250;
  });
}

// Run step, then note whether the engine asked the model on it: a
// request held before and none new after means what the step ended (a
// call, or a steer) waits for the held request's answer.
async function mayAsk(c: Ctx, step: () => Promise<void>): Promise<boolean> {
  const before = c.held.length;
  const next = c.next;
  await step();
  return before > 0 && c.next === next;
}

async function reply(c: Ctx, tool: 'js' | 'bash'): Promise<void> {
  const wait = `${WAIT} ${JSON.stringify(c.sibFile)} ]; do sleep 0.02; done`;
  const sib: Call = tool === 'js'
    ? { id: 'sib_w1', name: 'run_js', args: { code: `const r = tools.bash(${JSON.stringify(wait)} + '; cat ' + ${JSON.stringify(c.sibFile)});\nif (r.includes('fail')) throw new Error('sibling failed');\n'sibling ok';` } }
    : { id: 'sib_w1', name: 'bash', args: { command: `${wait}; grep -q fail ${JSON.stringify(c.sibFile)} && exit 3; echo sibling ok` } };
  const ask: Call = { id: 'ask_w1', name: 'ask', args: { question: 'Which colour?', options: ['red', 'blue'] } };
  answer(c, { mode: 'block', calls: [ask, sib] });
  await new Promise((r) => setTimeout(r, GRACE_MS));
  await settle(c);
}

async function siblingEnd(c: Ctx, end: 'ok' | 'fail'): Promise<void> {
  if (await mayAsk(c, async () => {
    const from = entries(c).length;
    c.sibSaid = end;
    fs.writeFileSync(c.sibFile, end);
    await waitEntry(c, from, "the sibling's end", (e) =>
      e.kind === 'result' || (e.kind === 'call' && e.data?.id === 'sib_w1' && e.data?.phase !== 'start'));
    await settle(c);
  })) c.unsent = true;
}

// ── what the page shows ─────────────────────────────────────────────

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();

const WORDS: Record<string, string> = {
  Running: 'running', 'Waiting for you': 'needs-you', Done: 'done', Stopped: 'stopped', Failed: 'error',
};

// One synchronous read of the page, so a question leaving mid-read
// cannot leave a locator waiting on an element that is gone.
async function dom(c: Ctx) {
  return c.page.evaluate(({ id, wait, perr, refusal }) => {
    const row = document.querySelector(`button.row[data-id="${id}"]`);
    const t = document.querySelector<HTMLElement>('.transcript');
    const card = t?.querySelector('.ask') ?? null;
    const label = (el: Element) => el.querySelector(':scope > summary .block-label')?.textContent?.trim() ?? '';
    const summary = (el: Element | null, sel: string) => Boolean(el?.querySelector(`:scope > summary ${sel}`));
    const thrown = (el: Element | null) => el?.querySelector(':scope > summary .tool-thrown')?.textContent ?? '';
    // The ask's own call, and the sibling's: its command is the wait.
    const askEl = [...(t?.querySelectorAll('details.call-native') ?? [])].find((el) => /^Ask(ing)?$/.test(label(el))) ?? null;
    const sibEl = [...(t?.querySelectorAll('details.toolcall') ?? [])]
      .find((el) => el.querySelector(':scope > summary .block-detail')?.textContent?.includes(wait)) ?? null;
    // Everything the person said, in order; errors count from the last.
    const said = [...(t?.querySelectorAll('.prompt-bubble') ?? [])];
    const lastSaid = said[said.length - 1];
    const after = (el: Element) => !lastSaid || Boolean(lastSaid.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING);
    const errs = [...(t?.querySelectorAll('.err-card') ?? [])].filter(after).map((e) => e.textContent ?? '');
    const answered = [...(t?.querySelectorAll('.meta-line') ?? [])].map((m) => m.textContent?.trim() ?? '')
      .filter((m) => m.startsWith('You answered: '));
    const options = [...(card?.querySelectorAll<HTMLButtonElement>('.ask-option') ?? [])];
    return {
      label: row?.getAttribute('aria-label') ?? '',
      card: Boolean(card),
      answerable: options.length > 0 && options.every((b) => !b.disabled),
      ask: askEl && {
        running: summary(askEl, '.tool-running'),
        job: [...askEl.querySelectorAll(':scope > summary .call-badge')].some((b) => /^Job \d+/.test(b.textContent ?? '')),
        cancelled: summary(askEl, '.tool-stopped'),
        thrown: thrown(askEl),
      },
      sib: sibEl && {
        native: sibEl.classList.contains('call-native'),
        running: summary(sibEl, '.tool-running'),
        failed: sibEl.classList.contains('block-failed'),
        cancelled: summary(sibEl, '.tool-stopped'),
      },
      perr: errs.some((e) => e.includes(perr)),
      noted: errs.some((e) => e.includes(refusal)),
      msgs: said.map((s) => s.textContent?.trim() ?? '').filter((s) => /^msg-\d+$/.test(s)),
      msgAnswered: answered.some((a) => /^You answered: msg-\d+$/.test(a)),
      notSent: [...document.querySelectorAll('.send-failed-gist')].some((g) => /msg-\d+/.test(g.textContent ?? '')),
    };
  }, { id: c.id, wait: WAIT, perr: PERR, refusal: REFUSAL });
}

function askerOf(d: Awaited<ReturnType<typeof dom>>): Asker | string {
  const a = d.ask;
  if (!a) return 'none';
  if (a.job) return 'job';
  if (a.running) return d.card ? 'open' : 'pending';
  if (a.cancelled) return 'cancelled';
  if (a.thrown.includes('no answer after')) return 'timedout';
  if (a.thrown) return `ended: ${a.thrown}`;
  return 'answered';
}

function siblingOf(d: Awaited<ReturnType<typeof dom>>): string {
  const s = d.sib;
  if (!s) return 'none';
  if (s.running) return 'running';
  if (s.cancelled) return 'cancelled';
  return s.failed ? 'failed' : 'ok';
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const d = await dom(c);
  const asker = askerOf(d);
  const sent = d.msgs.length > 0 || d.msgAnswered || d.notSent;
  return {
    status: WORDS[d.label.split(', ')[1]] ?? `unknown: ${d.label}`,
    asker,
    // serve's arm is the question's buttons taking an answer. The child's
    // hlAsk has no mark of its own (ArmsNameTheOpenAsk ties it to the
    // arm): one left behind shows on the next message, as msgAnswered.
    armed: d.card && d.answerable,
    hl: d.card && d.answerable,
    ask: d.card,
    sibling: siblingOf(d),
    tool: d.sib ? (d.sib.native ? 'bash' : 'js') : '',
    // Not on the page: the test's llm holds every request the engine
    // makes, so it knows which one is in flight and what waits for it.
    inflight: c.held.length > 0,
    queued: c.queued,
    unsent: c.unsent,
    perr: d.perr,
    noted: d.noted,
    sent,
    // The message went to the child as a message while the page showed
    // an ask still waiting.
    steeredPast: d.msgs.length > 0 && (c.askerAtSend === 'open' || c.askerAtSend === 'job'),
    msgAnswered: d.msgAnswered,
    lost: c.lost,
  };
}

const FLOW = {
  spec: 'ask_beside_parallel_calls',
  role: 'Session#0',
  config: CONTROL_CONFIG + '- id: loop\n  plugin: engine-unreal\n  config:\n    tools: both\n',
  env: { BOUGH_TEST_ASK_EXPIRE_DIR: expireDir },

  async init(page: Page, serve: Serve): Promise<Ctx> {
    // A walk is up to a dozen steps, each a real model round, and a
    // reply waits out the harness's one-second grace.
    test.info().setTimeout(180_000);
    const c: Ctx = {
      page, serve, id: '', dir: controlDir(serve.home), turn: 0, n: 0, next: '', held: [],
      sibFile: path.join(serve.work, '.sibling'), sibSaid: '', unsent: false, queued: false, askerAtSend: '', lost: false,
    };
    // The question is not put in until AskOpens removes this
    // (plugins/ask/native.go): the spec's asker "pending".
    fs.writeFileSync(path.join(expireDir, 'hold'), '');
    queueNext(c);
    c.id = await serve.newSession(`start ${c.next}`);
    await waitTaken(c.dir, c.next);
    absorb(c);
    await until('running', async () => (await sessionRow(c)).status === 'running');
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await row(c).waitFor();
    return c;
  },

  actions: {
    ReplyWithAskAndRunJS: (c: Ctx) => reply(c, 'js'),
    ReplyWithAskAndBash: (c: Ctx) => reply(c, 'bash'),

    async AskOpens(c: Ctx) {
      const from = entries(c).length;
      fs.rmSync(path.join(expireDir, 'hold'));
      await waitEntry(c, from, 'the ask', (e) => e.kind === 'ask');
      await settle(c);
    },

    SiblingEndOk: (c: Ctx) => siblingEnd(c, 'ok'),
    SiblingEndFail: (c: Ctx) => siblingEnd(c, 'fail'),

    async ProviderFails(c: Ctx) {
      const from = entries(c).length;
      answer(c, { mode: 'error', error: PERR });
      await waitEntry(c, from, 'the provider error', (e) => e.kind === 'error' && said(e).includes(PERR));
      await settle(c);
    },

    // A line on the child's stderr (llm-control prints it on cue), which
    // serve relays to the page as a live "error".
    async StderrLine(c: Ctx) {
      const name = path.join(c.dir, `s${++c.n}.stderr`);
      fs.writeFileSync(name + '-tmp', `abpc stderr line ${c.n}`);
      fs.renameSync(name + '-tmp', name);
      await until('the stderr line', () => fs.existsSync(name + '-said'));
      await settle(c);
    },

    // The request in flight is refused: an error note mid-turn.
    async HookErrorNote(c: Ctx) {
      const from = entries(c).length;
      answer(c, { mode: 'refuse', error: REFUSAL });
      await waitEntry(c, from, 'the refusal note', (e) => e.kind === 'error' && said(e).includes(REFUSAL));
      await settle(c);
    },

    // The question's first option. A refused answer (409) changes
    // nothing, and the page says so beside the composer.
    async Answer(c: Ctx) {
      if (await mayAsk(c, async () => {
        const from = entries(c).length;
        const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/answer`));
        await c.page.locator('.transcript .ask .ask-option').first().click();
        if ((await res).status() === 200) await waitEntry(c, from, 'the answer', (e) => e.kind === 'ask/answer');
        await settle(c);
      })) c.unsent = true;
    },

    async SendMessage(c: Ctx) {
      c.askerAtSend = askerOf(await dom(c)) as Asker;
      const before = await sessionRow(c);
      const text = `msg-${++c.n}`;
      if (await mayAsk(c, async () => {
        const from = entries(c).length;
        const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/prompt`));
        await c.page.locator('#composer').fill(text);
        await c.page.locator('.composer .btn-primary').click();
        if ((await res).status() === 200) {
          await waitEntry(c, from, 'the message', (e) => (e.kind === 'input' || e.kind === 'ask/answer') && said(e).includes(text));
          // A new turn: its request is held.
          if (before.status !== 'running') await until('running', async () => (await sessionRow(c)).status === 'running');
        }
        await settle(c);
      }) && before.status === 'running') c.queued = true;
    },

    // The ask's ten minutes run out, on cue.
    async Timeout(c: Ctx) {
      const d = await dom(c);
      if (!(d.card && d.answerable && askerOf(d) === 'open')) c.lost = true;
      if (await mayAsk(c, async () => {
        const from = entries(c).length;
        const id = String(entries(c).filter((e) => e.kind === 'ask').pop()?.data?.id ?? '');
        fs.writeFileSync(path.join(expireDir, id), '');
        await waitEntry(c, from, "the ask's timed-out end", (e) => e.kind === 'call' && e.data?.id === 'ask_w1' && e.data?.phase !== 'start');
        await settle(c);
      })) c.unsent = true;
    },

    async Stop(c: Ctx) {
      await c.page.getByRole('button', { name: 'Stop', exact: true }).click({ timeout: 10_000 });
      await until('the stop', async () => {
        const r = await sessionRow(c);
        return (r.status === 'stopped' || r.status === 'error') && !r.live;
      });
      // The child is gone and its requests with it.
      for (const h of c.held) release(c.dir, h);
      c.held = [];
      c.unsent = c.queued = false;
      await settle(c);
    },

    // Every held request answers until the turn ends.
    async Finish(c: Ctx) {
      await until('the turn to end', async () => {
        absorb(c);
        if ((await sessionRow(c)).status !== 'running') return true;
        if (c.held.length) answer(c);
        return false;
      });
      await settle(c);
    },
  },

  read: readUiState,
  status: row,
  sessions: (c: Ctx) => [c.id],

  // What the screen owes whatever the path: the question on screen is
  // the row's "Waiting for you", and the composer offers to answer it.
  async invariants(c: Ctx, where: string) {
    const d = await dom(c);
    expect(d.card, `${where}: the question card and the row disagree (${d.label})`).toBe(d.label.split(', ')[1] === 'Waiting for you');
    if (d.card) await expect(c.page.locator('.composer .btn-primary'), `${where}: the composer does not answer the question`).toHaveAccessibleName('Answer');
  },

  async cleanup(c: Ctx) {
    // The serve's work dir is gone once a timed-out test tore it down.
    if (!c.sibSaid && fs.existsSync(path.dirname(c.sibFile))) fs.writeFileSync(c.sibFile, 'ok');
    for (const h of c.held) release(c.dir, h);
    for (const f of fs.readdirSync(expireDir)) fs.rmSync(path.join(expireDir, f), { force: true });
  },
};

modelTests<Ctx>(FLOW);
