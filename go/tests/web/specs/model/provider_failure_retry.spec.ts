// go/tests/model/specs/provider_failure_retry.fizz in the browser
// (recipe: go/tests/model/README.md): one web session whose every model
// request is an llm-control "api" turn through the real Messages API
// adapter, so a 529 is retried by the adapter's own loop, a context
// overflow is a real 400 the Gate makes sticky, a refusal and a
// max_tokens stop are the provider's stop reasons. The Go twin is
// go/tests/model/mbt/provider_failure_retry_test.go; the provider's side
// of each step is driven the same way here, the person's through the
// page (the composer, the failed turn's Retry and Switch model, Stop).
//
// What is read off the DOM:
//
//   status   the sidebar row's label word (a failure: its red line)
//   live     the sidebar row's data-live
//   open     the last turn has no footer yet
//   closed   the last turn's footer: Stopped | any other outcome
//   dones    footers on the last turn
//   erred    an error card (or error line) in the last turn
//   stray    an error drawn in a turn that neither a prompt nor a wake
//            opened
//   turn     the last turn opens on a prompt bubble or a wake note
//   stream   the attempt whose fragments the live reply shows
//   cut      the max_tokens note is on screen in the last turn, not
//            folded away
//   call     the call's row: running or ended, with or without its Job
//            badge; a running call adopted when its turn failed has no
//            row until it ends, only its turn footer's "1 call still
//            running" (the Gate's parked / unparked is not on the page:
//            within one shape the walk's record says which)
//
// And what the page does not show, so this file keeps the spec's record
// of it, as the Go adapter does: req's out vs retry_wait (the hiccup is
// never drawn; streaming is read, it is "fragments on screen"), attempt,
// overflow, and paid (how many overflow 400s llm-control served).
//
// One step runs ahead: a Prompt on a model whose overflow is sticky is
// answered by the Gate at once, so the page never shows the spec's
// running state between the input and StickyOverflow. That step reads
// the input on the page and reports the running state as the spec's; the
// StickyOverflow step after it reads the real page.
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

interface Answer { kind: 'ok' | 'transient' | 'fatal' | 'overflow' | 'refused' | 'max_tokens'; text?: string; calls?: { id?: string; name: string; args?: Record<string, unknown> }[] }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;       // llm-control's queue
  n: number;         // names are unique across the walk
  held: string;      // the api turn of the request in flight, '' when none
  sentry: string;    // a turn queued for a request the Gate should answer
  gateF: string;     // the bash call's gate file, '' when none
  callMark: string;  // a word only the call's command carries
  frags: { text: string; attempt: number }[]; // fragments streamed this request
  ahead: boolean;    // the overflowed Prompt's step (see above)
  model: number;     // the session's model is control-<model>, 0: "control"
  // The spec's record of what the page does not show.
  req: string; attempt: number; call: string; overflow: boolean; paid: number;
}

const name = (c: Ctx, p: string) => `${p}${String(++c.n).padStart(5, '0')}`;
const bound = 15_000;

// --- llm-control's api turns (go/tests/model/llm/control.go) ---

function put(file: string, body: string): void {
  fs.writeFileSync(file + '-tmp', body);
  fs.renameSync(file + '-tmp', file);
}
// helpers/control.ts's Turn has no api mode; the row reads the same JSON.
const queueApi = (c: Ctx, turn: string, answer?: Answer) => put(path.join(c.dir, turn + '.json'), JSON.stringify({ mode: 'api', answer }));
const answerWith = (c: Ctx, turn: string, a: Answer) => put(path.join(c.dir, turn + '.answer'), JSON.stringify(a));

async function until<T>(what: string, fn: () => Promise<T | undefined | false> | T | undefined | false, ms = bound): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v !== undefined && v !== false) return v as T;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 20));
  }
}
const exists = (c: Ctx, f: string) => fs.existsSync(path.join(c.dir, f));
const waitAttempt = (c: Ctx, turn: string, k: number) => until(`attempt ${k} of ${turn}`, () => exists(c, `${turn}.attempt-${k}`));

async function streamText(c: Ctx, text: string): Promise<void> {
  const dst = path.join(c.dir, c.held + '.stream');
  put(dst, text);
  await until(`${c.held} to stream`, () => !fs.existsSync(dst));
}

async function apiRow(c: Ctx): Promise<{ status: string; live: boolean }> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  if (!res.ok()) throw new Error(`session: ${res.status()}`);
  return (await res.json()).session;
}
const settled = (c: Ctx) => until('the turn to end', async () => (await apiRow(c)).status !== 'running');

// A sentry the provider was asked for after all answered 400 again: paid.
function checkSentry(c: Ctx): void {
  if (!c.sentry) return;
  if (exists(c, c.sentry + '.taken')) c.paid++;
  else fs.rmSync(path.join(c.dir, c.sentry + '.json'), { force: true });
  c.sentry = '';
}

// --- readUiState ---

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const WORDS: Record<string, string> = { Idle: 'idle', Running: 'running', Done: 'done', Stopped: 'stopped' };

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const label = (await row(c).getAttribute('aria-label')) ?? '';
  const failed = (await row(c).locator('.row-meta-bad').count()) > 0;
  const status = WORDS[label.split(', ')[1] ?? ''] ?? (failed ? 'error' : `unknown: ${label}`);
  const live = (await row(c).getAttribute('data-live')) !== null;

  const dom = await c.page.evaluate(({ mark, frags }) => {
    const sections = [...document.querySelectorAll('.transcript section.turn')] as HTMLElement[];
    const opener = (t: HTMLElement) => (t.querySelector(':scope > .prompt') ? 'user' : t.querySelector(':scope > .job-wake') ? 'wake' : '');
    // A /model line is a section of its own with no prompt: not a turn.
    const turns = sections.filter((t) => opener(t) !== '');
    const last = turns[turns.length - 1];
    const errs = (t: HTMLElement) => t.querySelectorAll('.err-card, .err').length;
    const visible = (e: Element) => (e as HTMLElement).offsetParent !== null || (e as HTMLElement).getClientRects().length > 0;
    const call = [...document.querySelectorAll('.transcript details.call-native')].find((d) => mark && (d.textContent ?? '').includes(mark));
    const streamText = [...document.querySelectorAll('.transcript .stream-say, .transcript .thinking.stream-tip')].map((e) => e.textContent ?? '').join('\n');
    const cutNote = last ? [...last.querySelectorAll('*')].some((e) => e.children.length === 0 && (e.textContent ?? '').includes('cut off at max_tokens') && visible(e)) : false;
    return {
      any: turns.length > 0,
      opener: last ? opener(last) : '',
      feet: last ? last.querySelectorAll(':scope > .turn-foot').length : 0,
      stopped: last ? last.querySelector(':scope > .turn-foot .turn-stopped') !== null : false,
      erred: last ? errs(last) > 0 : false,
      stray: sections.some((t) => opener(t) === '' && errs(t) > 0),
      shown: frags.filter((f) => streamText.includes(f.text)).map((f) => f.attempt),
      cut: cutNote,
      // A running call adopted when its turn failed has no row until it
      // ends: its turn's footer counts it instead.
      call: call ? `${call.querySelector('.tool-running') ? 'running' : 'ended'}${[...call.querySelectorAll('.call-badge')].some((b) => /^Job /.test(b.textContent ?? '')) ? '-job' : ''}`
        : [...document.querySelectorAll('.transcript .turn-foot button')].some((b) => /still running/.test(b.textContent ?? '')) ? 'running-job' : 'absent',
    };
  }, { mark: c.callMark, frags: c.frags });

  const open = dom.any && dom.feet === 0;
  const stream = dom.shown.length ? Math.max(...dom.shown) : 0;
  // Within one row shape the Gate's state is the walk's record.
  const fits: Record<string, string[]> = {
    none: ['absent', 'ended', 'ended-job'], running: ['running'], returned: ['ended'],
    parked: ['running-job'], job: ['running-job'], ended: ['ended-job'],
  };
  const call = fits[c.call]?.includes(dom.call) ? c.call : `page shows ${dom.call}`;
  const state: Record<string, unknown> = {
    status,
    open,
    erred: dom.erred,
    closed: open || !dom.any ? '' : dom.stopped ? 'cancelled' : 'done',
    dones: dom.feet,
    stray: dom.stray,
    turn: dom.opener,
    req: !open ? 'none' : stream ? 'streaming' : c.req,
    attempt: c.attempt,
    stream,
    call,
    overflow: c.overflow,
    paid: c.paid,
    cut: dom.cut ? 'cut' : '',
    live,
  };
  if (c.ahead) Object.assign(state, { status: 'running', open: true, erred: false, closed: '', dones: 0, turn: 'user', req: 'out' });
  return state;
}

// --- steps ---

async function send(c: Ctx, text: string): Promise<void> {
  await c.page.locator('#composer').fill(text);
  await c.page.getByRole('button', { name: 'Send', exact: true }).click();
}

// An input from the page (a prompt, or the failed turn's Retry): its
// request is an api turn held at its first attempt; on an overflowed
// model the Gate answers it, so the turn queued for it is a sentry.
async function input(c: Ctx, go: () => Promise<void>): Promise<void> {
  const turn = name(c, 't');
  if (c.overflow) {
    c.sentry = turn;
    queueApi(c, turn, { kind: 'overflow' });
  } else {
    c.held = turn;
    queueApi(c, turn);
  }
  await go();
  if (c.call === 'parked') c.call = 'job';
  else if (c.call === 'ended' || c.call === 'returned') c.call = 'none';
  c.req = 'out';
  c.attempt = 1;
  c.frags = [];
  if (c.overflow) {
    // The Gate's answer is StickyOverflow: this step's read is the spec's.
    await until('the prompt on the page', async () => (await c.page.locator('.transcript section.turn .prompt-bubble').count()) > 0);
    c.ahead = true;
    return;
  }
  await waitAttempt(c, c.held, 1);
}

async function fail(c: Ctx, a: Answer): Promise<void> {
  answerWith(c, c.held, a);
  c.held = '';
  await failed(c);
}

async function failed(c: Ctx): Promise<void> {
  await settled(c);
  checkSentry(c);
  if (c.call === 'running') c.call = 'parked';
  c.req = 'none';
  c.attempt = 1;
  c.frags = [];
}

async function end(c: Ctx, a: Answer): Promise<void> {
  answerWith(c, c.held, a);
  c.held = '';
  await settled(c);
  c.req = 'none';
  c.attempt = 1;
  c.frags = [];
}

function openGate(c: Ctx): void {
  fs.writeFileSync(c.gateF, '');
  c.gateF = '';
}

// Another client archives the session (its child exits) and brings it
// back: the Go adapter's kill, a child gone between turns.
async function restart(c: Ctx): Promise<void> {
  let res = await c.serve.api.post(`/api/sessions/${c.id}/archive`, { data: {} });
  if (!res.ok()) throw new Error(`archive: ${res.status()}`);
  await until('the child to exit', async () => !(await apiRow(c)).live);
  res = await c.serve.api.post(`/api/sessions/${c.id}/unarchive`, { data: {} });
  if (!res.ok()) throw new Error(`unarchive: ${res.status()}`);
}

modelTests<Ctx>({
  spec: 'provider_failure_retry',
  role: 'Session#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    // A walk is ~50 steps, each a few seconds at worst.
    test.setTimeout(300_000);
    const c: Ctx = {
      page, serve, id: await serve.newSession(), dir: controlDir(serve.home), n: 0,
      held: '', sentry: '', gateF: '', callMark: '', frags: [], ahead: false, model: 0,
      req: 'none', attempt: 1, call: 'none', overflow: false, paid: 0,
    };
    fs.mkdirSync(c.dir, { recursive: true });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await page.locator('#composer').waitFor();
    return c;
  },

  actions: {
    // --- the person ---
    async Prompt(c) {
      await input(c, () => send(c, `prompt ${name(c, 'p')}`));
    },
    // The failed turn's card: Retry resends its prompt.
    async Retry(c) {
      await input(c, () => c.page.locator('.err-card').last().getByRole('button', { name: 'Retry', exact: true }).click());
    },
    // The failed turn's card: Switch model opens the composer's picker.
    async SwitchModel(c) {
      c.model++;
      const model = `control-${c.model}`;
      await c.page.locator('.err-card').last().getByRole('button', { name: 'Switch model', exact: true }).click();
      const res = await c.serve.api.post(`/api/sessions/${c.id}/model`, { data: { model } });
      if (!res.ok()) throw new Error(`model: ${res.status()}`);
      await c.page.keyboard.press('Escape');
      c.overflow = false;
      c.paid = 0;
    },
    // The composer's Stop while the adapter waits to retry: a SIGINT, the
    // turn closes cancelled and the child exits, taking any job with it.
    async StopDuringRetry(c) {
      await c.page.getByRole('button', { name: 'Stop', exact: true }).click();
      await settled(c);
      await until('the child to exit', async () => !(await apiRow(c)).live);
      if (c.call === 'running' || c.call === 'job') {
        c.call = 'none';
        c.gateF = '';
      }
      c.held = '';
      c.req = 'none';
      c.attempt = 1;
      c.frags = [];
    },

    // --- the provider ---
    async StreamFragment(c) {
      const text = name(c, 'f');
      await streamText(c, `${text} `);
      c.frags.push({ text, attempt: c.attempt });
      c.req = 'streaming';
    },
    async TransientError(c) {
      answerWith(c, c.held, { kind: 'transient' });
      await until(`${c.held} to wait for its retry`, () => exists(c, c.held + '.waiting'));
      c.attempt++;
      c.req = 'retry_wait';
    },
    async RetryFires(c) {
      fs.writeFileSync(path.join(c.dir, c.held + '.retry'), '');
      await waitAttempt(c, c.held, c.attempt);
      c.req = 'out';
      c.frags = [];
    },
    RetriesExhausted: (c) => fail(c, { kind: 'transient' }),
    FatalError: (c) => fail(c, { kind: 'fatal' }),
    async ContextOverflow(c) {
      c.overflow = true;
      c.paid++;
      await fail(c, { kind: 'overflow' });
    },
    async StickyOverflow(c) {
      c.ahead = false;
      await failed(c);
    },
    // A bash call that runs until its gate file appears, and a call of a
    // tool that does not exist, whose result sends the next request. The
    // mark leads the command: a live call row carries it clipped.
    async CallStart(c) {
      const next = name(c, 't');
      c.callMark = `${next}-gate`;
      c.gateF = path.join(c.serve.home, c.callMark);
      queueApi(c, next);
      answerWith(c, c.held, { kind: 'ok', calls: [
        { id: `toolu_${next}_run`, name: 'bash', args: { command: `: ${c.callMark}; while [ ! -e ${c.gateF} ]; do sleep 0.05; done; echo gate open` } },
        { id: `toolu_${next}_quick`, name: 'no_such_tool' },
      ] });
      c.held = next;
      await waitAttempt(c, next, 1);
      c.call = 'running';
      c.req = 'out';
      c.attempt = 1;
      c.frags = [];
    },
    Finish: (c) => end(c, { kind: 'ok', text: `finished ${c.held}` }),
    Refusal: (c) => end(c, { kind: 'refused' }),
    MaxTokensStop: (c) => end(c, { kind: 'max_tokens', text: 'and then' }),

    // --- the system ---
    async CallEnds(c) {
      openGate(c);
      c.call = 'returned';
    },
    // The parked call's end starts nothing: no request, no wake turn.
    async CallEndsWhileParked(c) {
      openGate(c);
      await new Promise((r) => setTimeout(r, 500));
      c.call = 'ended';
    },
    // The unparked job ends while idle and the request its end starts
    // fails before the provider (sticky overflow: the sentry must stay
    // untaken) or at it (a 400).
    async WakeRequestFails(c) {
      const turn = name(c, 't');
      if (c.overflow) {
        c.sentry = turn;
        queueApi(c, turn, { kind: 'overflow' });
      } else {
        queueApi(c, turn, { kind: 'fatal' });
      }
      openGate(c);
      await waitTaken(c.dir, turn).catch(() => {});
      await until('the wake to fail', async () => (await c.page.locator('.transcript section.turn').last().locator(':scope > .job-wake').count()) > 0
        && (await apiRow(c)).status !== 'running');
      checkSentry(c);
      c.call = 'none';
    },
    ChildRestart: (c) => restart(c),
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.gateF) fs.writeFileSync(c.gateF, '');
    if (c.held) answerWith(c, c.held, { kind: 'ok', text: 'cleanup' });
  },
});
