// go/tests/model/specs/notice_during_open_ask.fizz in the browser (recipe:
// go/tests/model/README.md): a background agent's report, a watcher's
// wake and the page's model picker meeting a question the model has open
// (tools.ask, tools.secret) in a thread the person is looking at. The Go
// twin is go/tests/model/mbt/notice_during_open_ask_test.go; the model is
// llm-control, so every model request is one the walk holds and lets go.
//
// Only the person's steps have UI: Answer and AnswerNoticeLike (the
// question's option, its secret field, the composer's Answer), SetModel
// (the Next turn model picker) and Prompt (the composer). The rest are
// other parties and go where those parties write: the model's calls
// through llm-control, the ask timeout through BOUGH_TEST_ASK_EXPIRE_DIR,
// the report through POST /notify (the {"notice"} line children.go's
// report writes, without running a second agent for it), and the watcher
// through POST /prompt, which is supWaker.Wake's Send with only the same
// pending-ask refusal in front of it.
//
// Markers say which line ended up where on the page: NOTE- is the report
// (an agent named NOTE-n), PHANTOM- a person's answer that parses as
// {"notice"}, S3CR3T- the person's secret, "[watcher]" the wake.
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

const PROJECT = 'p1';
const WAKE_PREFIX = '[watcher] ';
const MODEL = 'nda-model-x';
const TIMEOUT = 30_000;

// One per worker: tests in a worker run one at a time, and the expire
// files are named by ask id, which is never reused.
const expireDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-nda-expire-'));
const keychainDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-nda-keychain-'));

test.use({
  serveOpts: {
    config: CONTROL_CONFIG,
    home: { [`.bough/projects/${PROJECT}/project.yml`]: `name: ${PROJECT}\n` },
    env: { BOUGH_TEST_ASK_EXPIRE_DIR: expireDir, BOUGH_TEST_KEYCHAIN_DIR: keychainDir },
  },
});
// A walk is up to a dozen steps, each a model request or two.
test.describe.configure({ timeout: 180_000 });

interface Entry { kind: string; data?: Record<string, unknown> }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;        // llm-control's queue
  turn: number;
  n: number;
  held: string;       // the model request held in flight, '' when none
  next: string;       // the block turn queued for the next request
  note: string;       // the report's agent name, '' before Notify
  noticeSeen: boolean; // a model request was taken after the report landed
  secretSent: boolean; // the page's secret answer was accepted
  wake: 'none' | 'queued' | 'checked' | 'sent';
  wakeText: string;
  wakeCode: number;
  steered: boolean;   // a wake steered the turn and waits for the request's boundary
  stray: string;      // a second request in flight: the engine broke its own contract
}

const q1 = { question: 'Which colour?', options: ['red', 'blue'] };
const q2 = { name: 'TOKEN', question: 'the API token', project: PROJECT };

// ── what the page shows ─────────────────────────────────────────────

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const composer = (c: Ctx) => c.page.locator('#composer');
const sendButton = (c: Ctx) => c.page.locator('.composer .btn-primary');
const modelSel = (c: Ctx) => c.page.locator('.composer-tools .sel').filter({ has: c.page.locator('button[aria-label^="Next turn model"]') });

const WORDS: Record<string, string> = {
  Running: 'running', 'Waiting for you': 'needs-you', Done: 'done',
};

// One synchronous read of the page, so a question leaving mid-read
// cannot leave a locator waiting on an element that is gone.
async function dom(c: Ctx) {
  return c.page.evaluate(({ id, note }) => {
    const row = document.querySelector(`button.row[data-id="${id}"]`);
    const card = document.querySelector('.transcript .ask');
    const labels = [...document.querySelectorAll('.transcript .call-native .block-label')].map((l) => l.textContent?.trim() ?? '');
    const modelBtn = [...document.querySelectorAll('.composer-tools button.sel-btn')].find((b) => b.getAttribute('aria-label')?.startsWith('Next turn model'));
    return {
      label: row?.getAttribute('aria-label') ?? '',
      ask: card ? (card.querySelector('input[aria-label="Secret value"]') ? 'secret' : 'plain') : '',
      plainAsked: labels.includes('Ask'),
      secretAsked: labels.includes('Secret'),
      // The report's rows: a note inside a turn, or the turn it woke.
      notices: note ? [...document.querySelectorAll('.transcript .agent-notice .agent-notice-name')].filter((e) => e.getAttribute('title') === note).length : 0,
      // Rows the transcript shows as a background job's or agent's note.
      jobRows: [...document.querySelectorAll('.transcript details.block.thin > summary')]
        .filter((s) => !s.closest('.call-native')).map((s) => s.textContent ?? ''),
      // A turn's work folds: its answer lines are one click in.
      meta: [...document.querySelectorAll('.transcript .meta-line')].map((m) => m.textContent?.trim() ?? ''),
      // What was said to the agent: turn prompts and steers.
      prompts: [...document.querySelectorAll('.transcript .prompt-bubble')].map((p) => p.textContent ?? ''),
      model: modelBtn?.getAttribute('aria-label') ?? '',
      saveFailed: !!document.querySelector('.composer-tools .sel-save-failed'),
    };
  }, { id: c.id, note: c.note });
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  absorb(c);
  if (c.stray) throw new Error(c.stray);
  const d = await dom(c);
  const answers = d.meta.map((m) => /^You answered: ([\s\S]*)$/.exec(m)?.[1]).filter((t): t is string => t !== undefined);
  const stored = d.meta.includes('secret stored');

  // The report is on the page once job-notices has it; the model has it
  // once a request went out after that (not on the page: the walk sees
  // llm-control take it, as the Go adapter does).
  let notice = 'none', noticeCount = d.notices;
  if (d.notices > 0) {
    notice = c.noticeSeen ? 'delivered' : 'queued';
    if (!c.noticeSeen) noticeCount--;
  }

  // The wake: queued and checked are the watcher's own; once sent, the
  // page shows it as a turn's prompt, or serve refused it.
  let wake: string = c.wake;
  if (wake === 'sent') {
    if (c.wakeCode === 409) wake = 'dropped';
    else if (d.prompts.some((p) => p.includes(WAKE_PREFIX + c.wakeText))) wake = 'delivered';
    else wake = `lost(${c.wakeCode})`;
  }

  return {
    status: WORDS[d.label.split(', ')[1]] ?? `unknown: ${d.label}`,
    ask: d.ask,
    plainAsked: d.plainAsked,
    secretAsked: d.secretAsked,
    notice,
    noticeCount,
    wake,
    // The picker names what the next turn runs; a refusal leaves it
    // saying it could not save.
    model: d.model.endsWith(': ' + MODEL) ? 'applied' : d.saveFailed ? 'refused' : 'none',
    // The secret itself is never on the page: "secret stored" is, and
    // whether the page sent one.
    secret: stored ? (c.secretSent ? 'person' : 'notice') : '',
    noticeAnswered: answers.some((t) => t.includes('NOTE-')) || (stored && !c.secretSent),
    phantom: d.jobRows.some((t) => t.includes('PHANTOM-')),
    strayLine: answers.some((t) => t.startsWith(WAKE_PREFIX) || t.startsWith('/')),
  };
}

// ── driving the model ───────────────────────────────────────────────

function queueNext(c: Ctx): void {
  c.next = `n${String(++c.turn).padStart(5, '0')}`;
  queue(c.dir, c.next, { mode: 'block', text: `finished ${c.next}` });
}

function entries(c: Ctx): Entry[] {
  const file = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));
}

const text = (e: Entry) => String(e.data?.text ?? '');

// The report's arrivals in the loop: a "job" note inside an open turn, or
// the input of the turn it woke an idle agent with.
function landed(c: Ctx, es: Entry[]): number {
  return es.filter((e) => c.note && (e.kind === 'job' || (e.kind === 'input' && e.data?.reason === 'notice')) && text(e).includes(c.note)).length;
}

// A model request the engine made on its own (a notice or an answer's
// result going out), held as the walk's.
function absorb(c: Ctx): boolean {
  if (!fs.existsSync(path.join(c.dir, c.next + '.taken'))) return false;
  if (c.held && !c.stray) c.stray = `request ${c.next} taken while ${c.held} was still in flight`;
  c.held = c.next;
  queueNext(c);
  if (c.note && landed(c, entries(c)) > 0) c.noticeSeen = true;
  return true;
}

async function until(what: string, ok: () => Promise<boolean> | boolean): Promise<void> {
  const deadline = Date.now() + TIMEOUT;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}

const waitRequest = (c: Ctx, why: string) => until(`the model request after ${why}`, () => absorb(c));

async function waitHistory(c: Ctx, n: number, what: string, ok: (e: Entry) => boolean): Promise<void> {
  await until(what, () => entries(c).slice(n).some(ok));
}

async function apiRow(c: Ctx): Promise<{ status: string; ask?: { id: string; secret?: boolean } }> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  return (await res.json()).session;
}

async function waitStatus(c: Ctx, status: string): Promise<void> {
  await until(`the row to be ${status}`, async () => (await apiRow(c)).status === status);
}

// A report waiting at the boundary of the request just released goes
// out with the engine's next request.
async function settleNotice(c: Ctx, why: string): Promise<void> {
  if (c.note && !c.noticeSeen && landed(c, entries(c)) > 0 && !c.held) await waitRequest(c, why);
}

// The model calls tools.ask / tools.secret.
async function askTool(c: Ctx, tool: 'ask' | 'secret', args: Record<string, unknown>): Promise<void> {
  releaseWith(c.dir, c.held, { mode: 'call', tool, args });
  c.held = '';
  await waitStatus(c, 'needs-you');
  if (c.steered && !c.held) {
    // A wake that steered the turn went out at this boundary: the model
    // answers it while its question stays open.
    await waitRequest(c, 'the steer');
    release(c.dir, c.held);
    c.held = '';
  }
  c.steered = false;
  await settleNotice(c, 'the ask');
}

// After the ask's call returns: its result goes out with the next
// request, unless one is in flight.
async function askEnded(c: Ctx, n: number, why: string): Promise<void> {
  await waitHistory(c, n, `the ask's call ending (${why})`, (e) => {
    const t = String(e.data?.tool ?? '');
    return (e.kind === 'call' && e.data?.phase !== 'start' && (t === 'ask' || t === 'secret')) || (e.kind === 'job' && text(e).includes('PHANTOM-'));
  });
  if (!c.held) await waitRequest(c, why);
}

// ── the page's requests ─────────────────────────────────────────────

// The composer's Answer, with the text typed in it.
async function answerInComposer(c: Ctx, t: string): Promise<void> {
  const n = entries(c).length;
  await composer(c).fill(t);
  const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/answer`));
  await sendButton(c).click();
  if ((await res).status() !== 200) throw new Error(`answer ${t}: ${(await res).status()}`);
  await askEnded(c, n, 'the answer');
}

modelTests<Ctx>({
  spec: 'notice_during_open_ask',
  role: 'Session#0',

  async init(page, serve) {
    fs.rmSync(path.join(keychainDir, `bough%${PROJECT}%TOKEN`), { force: true });
    const c: Ctx = {
      page, serve, id: '', dir: controlDir(serve.home), turn: 0, n: 0, held: '', next: '', note: '',
      noticeSeen: false, secretSent: false, wake: 'none', wakeText: '', wakeCode: 0, steered: false, stray: '',
    };
    // The picker offers the model the walk sets: llm-control lists none.
    await page.route('**/api/models', async (route) => {
      const res = await route.fetch();
      const cat = await res.json();
      cat.providers = [...(cat.providers ?? []), { plugin: 'llm-control', models: [{ id: MODEL }] }];
      await route.fulfill({ response: res, json: cat });
    });
    queueNext(c);
    c.id = await serve.newSession(`start ${c.next}`);
    await waitRequest(c, 'the first prompt');
    await waitStatus(c, 'running');
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await row(c).waitFor();
    return c;
  },

  actions: {
    AskPlain: (c) => askTool(c, 'ask', q1),
    AskSecret: (c) => askTool(c, 'secret', q2),

    // A background agent's turn closes and serve reports it.
    async Notify(c) {
      const n = entries(c).length;
      c.note = `NOTE-${++c.n}`;
      const res = await c.serve.api.post(`/api/sessions/${c.id}/notify`, { data: { text: `[agent ${c.note} · ${crypto.randomUUID()} finished] report ${c.note}` } });
      if (res.status() !== 200) throw new Error(`notify: ${res.status()} ${await res.text()}`);
      await waitHistory(c, n, 'the report reaching the loop or an ask', (e) =>
        e.kind === 'ask/answer' || ((e.kind === 'job' || e.kind === 'input') && text(e).includes(c.note)));
      if (c.held) return; // at the boundary of the request in flight
      await waitRequest(c, 'the report');
    },

    // The model's step in flight ends; the report goes out with the next.
    async DeliverNotice(c) {
      release(c.dir, c.held);
      c.held = '';
      c.steered = false;
      await waitRequest(c, 'the step the report waited for');
    },

    // The watcher's poll finds news and queues a wake; nothing on the
    // page until it is sent.
    async WatcherQueue(c) {
      c.wakeText = `news-${++c.n}`;
      c.wake = 'queued';
    },
    // deliver() finds the session idle (the page says Done; the harness
    // just read it) and is about to Send.
    async WatcherCheck(c) {
      c.wake = 'checked';
    },
    // Wake -> Send: refused on an armed ask; otherwise it steers the
    // turn in flight, or starts one.
    async WatcherSend(c) {
      const before = (await apiRow(c)).status;
      const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: WAKE_PREFIX + c.wakeText } });
      c.wake = 'sent';
      c.wakeCode = res.status();
      if (c.wakeCode !== 200) return;
      await waitHistory(c, 0, "the wake's input", (e) => e.kind === 'input' && text(e).includes(c.wakeText));
      if (before === 'done') {
        await waitRequest(c, 'the wake');
        await waitStatus(c, 'running');
        return;
      }
      c.steered = true;
    },

    // The Next turn model picker.
    async SetModel(c) {
      const n = entries(c).length;
      // A refused pick can leave the list open: a second click would shut it.
      const btn = modelSel(c).locator('button.sel-btn');
      if ((await btn.getAttribute('aria-expanded')) !== 'true') await btn.click();
      await modelSel(c).locator('input.sel-search').fill(MODEL);
      const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/model`) && r.request().method() === 'POST');
      await modelSel(c).getByRole('option').filter({ has: c.page.locator('.sel-label', { hasText: MODEL }) }).click();
      const status = (await res).status();
      if (status === 409) return;
      if (status !== 200) throw new Error(`set model: ${status}`);
      await waitHistory(c, n, 'the child applying /model', (e) => e.kind === 'model' && JSON.stringify(e.data).includes(MODEL));
    },

    // The person answers the question on screen: an option, or the
    // secret field.
    async Answer(c) {
      const n = entries(c).length;
      const secret = c.page.getByLabel('Secret value');
      const res = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/answer`));
      const isSecret = (await secret.count()) > 0;
      if (isSecret) {
        await secret.fill(`S3CR3T-${++c.n}`);
        await secret.press('Enter');
      } else {
        await c.page.locator('.transcript .ask').getByRole('button', { name: 'red', exact: true }).click();
      }
      const status = (await res).status();
      if (status !== 200) throw new Error(`answer: ${status}`);
      if (isSecret) c.secretSent = true;
      await askEnded(c, n, 'the answer');
    },

    // The same with a reply whose text parses as {"notice": ...}.
    AnswerNoticeLike: (c) => answerInComposer(c, `{"notice":"PHANTOM-${++c.n}"}`),

    // The open ask times out, as its timeout would.
    async Timeout(c) {
      const n = entries(c).length;
      const id = (await apiRow(c)).ask?.id;
      if (!id) throw new Error('timeout: no ask open');
      fs.writeFileSync(path.join(expireDir, id), '');
      await askEnded(c, n, 'the timeout');
    },

    // The turn ends: whatever lands at a request's boundary (a report, a
    // steer) makes one more request, which is let go too.
    async Finish(c) {
      for (;;) {
        if (!c.held) throw new Error('finish: no model request is held');
        release(c.dir, c.held);
        c.held = '';
        c.steered = false;
        let done = false;
        await until('the turn to end or ask again', async () => {
          if (absorb(c)) return true;
          done = (await apiRow(c)).status === 'done';
          return done;
        });
        if (done) return;
      }
    },

    // The person prompts the finished session from the composer.
    async Prompt(c) {
      await composer(c).fill(`msg-${++c.n}`);
      await sendButton(c).click();
      await waitRequest(c, 'the prompt');
      await waitStatus(c, 'running');
    },
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  // The picker's refused POST /model (the spec's SetModel on an open
  // ask): Chromium logs the 409, the page says it could not save.
  allowConsole: /^Failed to load resource: the server responded with a status of 409 /,
  async cleanup(c) {
    if (c.held) release(c.dir, c.held);
  },
});
