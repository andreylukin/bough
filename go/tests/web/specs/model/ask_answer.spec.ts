// go/tests/model/specs/ask_answer.fizz in the browser (recipe:
// go/tests/model/README.md): the model asks a question (tools.ask, then
// tools.secret) and the person answers from the thread. The model is
// llm-control: an ask is a "call" turn, its timeout fires on cue through
// BOUGH_TEST_ASK_EXPIRE_DIR, and the secret lands in a file keychain.
// The Go twin is go/tests/model/mbt/ask_answer_test.go.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

const PROJECT = 'p1';

// One per worker: tests in a worker run one at a time, and the expire
// files are named by ask id, which is never reused.
const expireDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-ask-expire-'));
const keychainDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-ask-keychain-'));

test.use({
  serveOpts: {
    config: CONTROL_CONFIG,
    home: { [`.bough/projects/${PROJECT}/project.yml`]: `name: ${PROJECT}\n` },
    // The file keychain keeps the secret off the real one.
    env: { BOUGH_TEST_ASK_EXPIRE_DIR: expireDir, BOUGH_TEST_KEYCHAIN_DIR: keychainDir },
  },
});

type Q = 'q1' | 'q2';

interface Sent { verb: 'prompt' | 'answer'; ask?: string; text: string }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;               // llm-control's queue
  turn: number;
  held: string;              // the model request held in flight, '' when none
  next: string;              // the block turn queued for the next request
  n: number;                 // draft markers
  asks: Map<string, Q>;      // ask id -> question, as the model asked them
  last: string;              // the last ask id
  // The POST the page sent and the test holds back from serve (Deliver
  // lets it go): the spec's inflight.
  pending: { route: Route; sent: Sent } | null;
  types: number;             // Type steps taken
  delivered: { sent: Sent; status: number }[];
}

const q1 = { question: 'Which colour?', options: ['red', 'blue'] };
const q2 = { name: 'TOKEN', question: 'the API token', project: PROJECT };

// ── what the page shows ─────────────────────────────────────────────

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const secretField = (c: Ctx) => c.page.getByLabel('Secret value');
const composer = (c: Ctx) => c.page.locator('#composer');
const sendButton = (c: Ctx) => c.page.locator('.composer .btn-primary');

const WORDS: Record<string, string> = {
  Running: 'running', 'Waiting for you': 'needs-you', Done: 'done', Interrupted: 'interrupted',
};

async function onScreen(c: Ctx): Promise<'' | Q> {
  return (await dom(c)).ask;
}

// One synchronous read of the page, so a question leaving mid-read
// cannot leave a locator waiting on an element that is gone.
async function dom(c: Ctx) {
  return c.page.evaluate((id) => {
    const row = document.querySelector(`button.row[data-id="${id}"]`);
    const card = document.querySelector('.transcript .ask');
    const secret = card?.querySelector<HTMLInputElement>('input[aria-label="Secret value"]');
    const note = [...document.querySelectorAll('.composer-note-err')].find((n) => n.textContent?.includes('Question changed'));
    let draftAsk = '';
    try { draftAsk = localStorage.getItem(`bough:draft-ask:${id}`) ?? ''; } catch { /* storage off */ }
    return {
      label: row?.getAttribute('aria-label') ?? '',
      ask: (card ? (secret ? 'q2' : 'q1') : '') as '' | 'q1' | 'q2',
      secretValue: secret?.value ?? '',
      text: (document.querySelector<HTMLTextAreaElement>('#composer')?.value ?? '').trim(),
      note: note?.textContent ?? '',
      sendLabel: document.querySelector('.composer .btn-primary')?.getAttribute('aria-label') ?? '',
      draftAsk,
      transcript: document.querySelector<HTMLElement>('.transcript')?.innerText ?? '',
      // Every ask and secret call the model made, folded into a tool
      // group or not: the transcript keeps them after the card is gone.
      asked: [...document.querySelectorAll('.transcript .call-native .block-label')].filter((l) => /^(Ask|Secret)$/.test(l.textContent?.trim() ?? '')).length,
      meta: [...document.querySelectorAll<HTMLElement>('.transcript .meta-line')].map((m) => m.innerText.trim()),
    };
  }, c.id);
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const d = await dom(c);
  const status = WORDS[d.label.split(', ')[1]] ?? `unknown: ${d.label}`;
  const ask = d.ask;

  // The draft: the secret field is always the secret's answer; the
  // composer's is an answer when the page says so (the Answer button, or
  // the "Question changed" note for one whose question is gone).
  let draft = '';
  if (d.secretValue) draft = 'sec';
  else if (d.text) {
    if (d.note) {
      // Which gone question it answered is not on screen; the page keeps
      // the id it was written for with the draft.
      const id = d.note.includes('written as a message') ? '' : d.draftAsk;
      draft = id ? c.asks.get(id) ?? `unknown ask ${id}` : 'msg';
    } else {
      draft = d.sendLabel === 'Answer' ? ask : 'msg';
    }
  }

  // inflight: the request the page sent that serve has not had yet.
  let inflight = '';
  if (c.pending) {
    const s = c.pending.sent;
    inflight = s.verb === 'prompt' ? 'msg' : c.asks.get(s.ask ?? '') ?? `unknown ask ${s.ask}`;
    if (inflight === 'q2' && s.text.includes('\n')) inflight = 'q2nl';
  }

  // What the transcript betrays, against what the page sent: "You
  // answered: T" for a message, for text sent to a secret, or for text
  // never sent as an answer; a secret's text anywhere in it; a
  // secret's second line anywhere in it.
  const transcript = d.transcript;
  const answered = d.meta;
  const accepted = c.delivered.filter((d) => d.status === 200);
  const secrets = c.delivered.filter((d) => d.sent.verb === 'answer' && c.asks.get(d.sent.ask ?? '') === 'q2').map((d) => d.sent.text);
  let msgAnswered = false, wrongAnswer = false;
  for (const m of answered) {
    const said = /^You answered: ([\s\S]*)$/.exec(m.trim())?.[1];
    if (said === undefined) continue;
    if (accepted.some((d) => d.sent.verb === 'prompt' && d.sent.text === said)) msgAnswered = true;
    else if (!accepted.some((d) => d.sent.verb === 'answer' && c.asks.get(d.sent.ask ?? '') === 'q1' && d.sent.text === said)) wrongAnswer = true;
  }
  const stored = answered.filter((m) => m.trim() === 'secret stored').length;
  if (stored > accepted.filter((d) => d.sent.verb === 'answer' && c.asks.get(d.sent.ask ?? '') === 'q2').length) wrongAnswer = true;
  const secretLogged = secrets.some((s) => s.split('\n').some((part) => part && transcript.includes(part)));
  const split = secrets.some((s) => s.includes('\n') && transcript.includes(s.split('\n')[1]));

  return {
    status,
    // Not on the page: the spec's own invariants tie them to what is.
    // A child is gone exactly when its turn reads interrupted, and serve's
    // arm and the child's open ask are the question on screen
    // (ArmedMatchesScreen, NeedsYouOnlyWhileAlive).
    alive: status !== 'interrupted',
    asked: d.asked,
    open: ask,
    armed: ask,
    ask,
    draft,
    inflight,
    msgAnswered,
    wrongAnswer,
    secretLogged,
    split,
  };
}

// ── driving the model ───────────────────────────────────────────────

function queueNext(c: Ctx): void {
  c.next = `t${String(++c.turn).padStart(5, '0')}`;
  queue(c.dir, c.next, { mode: 'block', text: `finished ${c.next}` });
}

// The model's current request: the one the engine made after the last
// ask returned, a steer, or a new turn.
async function hold(c: Ctx): Promise<void> {
  if (c.held) return;
  await waitTaken(c.dir, c.next);
  c.held = c.next;
  queueNext(c);
}

async function sessionRow(c: Ctx): Promise<{ status: string; live: boolean }> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  return (await res.json()).session;
}

async function until(what: string, ok: () => Promise<boolean>): Promise<void> {
  const deadline = Date.now() + 15_000;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

function historyAsks(c: Ctx): { id: string; secret: boolean }[] {
  const file = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l))
    .filter((e) => e.kind === 'ask').map((e) => ({ id: String(e.data?.id), secret: Boolean(e.data?.secret) }));
}

// The model calls the tool; the test learns the ask's id from history,
// as the Go adapter does, to time it out and to name what the page sends.
async function askTool(c: Ctx, tool: 'ask' | 'secret', args: Record<string, unknown>, q: Q): Promise<void> {
  await hold(c);
  const before = historyAsks(c).length;
  releaseWith(c.dir, c.held, { mode: 'call', tool, args });
  c.held = '';
  await until(`the ${tool} entry`, async () => historyAsks(c).length > before);
  const a = historyAsks(c)[before];
  c.asks.set(a.id, q);
  c.last = a.id;
}

// ── the page's requests ─────────────────────────────────────────────

async function held(c: Ctx): Promise<void> {
  await until('the page to send', async () => c.pending !== null);
}

// A draft is one line, unless this path sends it as SendMultiline: the
// text is not part of the spec's state, and once it has become an answer
// to the secret the (disabled) composer cannot take a second line.
async function typeDraft(c: Ctx): Promise<void> {
  c.n++;
  if ((await onScreen(c)) === 'q2') {
    await secretField(c).fill(`S3CR3T-${c.n}`);
    c.types++;
    return;
  }
  const walk = test.info().title.replace(/^path \d+: /, '').split(' → ');
  const at = walk.map((a, i) => a === 'Type' ? i : -1).filter((i) => i >= 0)[c.types++];
  const until = walk.indexOf('Type', at + 1);
  const multi = walk.slice(at + 1, until < 0 ? undefined : until).includes('SendMultiline');
  await composer(c).fill(multi ? `draft-${c.n}\ndraft-${c.n}-tail` : `draft-${c.n}`);
}

modelTests<Ctx>({
  spec: 'ask_answer',
  role: 'Session#0',

  async init(page, serve) {
    fs.rmSync(path.join(keychainDir, `bough%${PROJECT}%TOKEN`), { force: true });
    const c: Ctx = {
      page, serve, id: '', dir: controlDir(serve.home), turn: 0, held: '', next: '', n: 0,
      asks: new Map(), last: '', pending: null, types: 0, delivered: [],
    };
    queueNext(c);
    c.id = await serve.newSession(`start ${c.next}`);
    await hold(c);
    // Every answer and message the page sends is held here until Deliver.
    await page.route(new RegExp(`/api/sessions/${c.id}/(prompt|answer)$`), (route) => {
      const verb = route.request().url().endsWith('/prompt') ? 'prompt' : 'answer';
      const body = JSON.parse(route.request().postData() ?? '{}');
      c.pending = { route, sent: { verb, ask: body.ask, text: body.text } };
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await row(c).waitFor();
    return c;
  },

  actions: {
    AskPlain: (c) => askTool(c, 'ask', q1, 'q1'),
    AskSecret: (c) => askTool(c, 'secret', q2, 'q2'),
    Type: typeDraft,
    UseForQuestion: (c) => c.page.getByRole('button', { name: 'Use for this question' }).click(),
    KeepAsMessage: (c) => c.page.getByRole('button', { name: 'Keep as message' }).click(),

    async Send(c) {
      if ((await dom(c)).secretValue) await secretField(c).press('Enter');
      else await sendButton(c).click();
      await held(c);
    },
    // Only a composer draft answers the secret this way, and the composer
    // is disabled while the secret is on screen: the draft's second line
    // was typed with it (see typeDraft).
    async SendMultiline(c) {
      await sendButton(c).click();
      await held(c);
    },

    // serve handles the POST: the held request goes on, and the page gets
    // serve's answer, a refusal included.
    async Deliver(c) {
      const p = c.pending!;
      const res = c.page.waitForResponse((r) => r.request() === p.route.request());
      await p.route.continue();
      const status = (await res).status();
      c.pending = null;
      c.delivered.push({ sent: p.sent, status });
    },

    // The open ask times out, as its timeout would.
    async Resolve(c) {
      fs.writeFileSync(path.join(expireDir, c.last), '');
      await until('the ask to resolve', async () => (await sessionRow(c)).status === 'running');
    },

    // The turn ends: a steer that landed meanwhile makes one more
    // request, which is let go too.
    async Finish(c) {
      await hold(c);
      for (;;) {
        release(c.dir, c.held);
        c.held = '';
        let done = false;
        await until('the turn to end or ask again', async () => {
          if (fs.existsSync(path.join(c.dir, c.next + '.taken'))) return true;
          done = (await sessionRow(c)).status === 'done';
          return done;
        });
        if (done) return;
        c.held = c.next;
        queueNext(c);
      }
    },

    // The child dies (a crash) with its question on screen.
    async Exit(c) {
      // lsof prints the resolved path (/private/var/…).
      const work = fs.realpathSync(c.serve.work);
      const out = execFileSync('lsof', ['-a', '-d', 'cwd', '-c', 'bough', '-Fpn'], { encoding: 'utf8' });
      let pid = 0;
      for (const l of out.split('\n')) {
        if (l.startsWith('p')) pid = Number(l.slice(1));
        else if (l.startsWith('n') && l.slice(1) === work && pid) process.kill(pid, 'SIGKILL');
      }
      c.held = '';
      await until('the child to be gone', async () => !(await sessionRow(c)).live);
    },
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  // A refusal the spec models (409, and 400 for a secret with a newline):
  // the page reports it beside the composer.
  allowConsole: /^Failed to load resource: the server responded with a status of (409|400) /,
  async cleanup(c) {
    await c.pending?.route.abort().catch(() => {});
    if (c.held) release(c.dir, c.held);
  },
});
