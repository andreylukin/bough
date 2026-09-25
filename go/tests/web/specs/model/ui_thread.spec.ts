// go/tests/model/specs/ui_thread.fizz in the browser: one open thread
// (#/s/<id>) as a person sees it and acts on it — the header's status,
// the Settings popover, the strip's Changes chip and Details overflow,
// the jump button, the running turn's "Working" fold and its call row,
// the error card's Retry, the "N files" link, and the Changes and
// Context sub pages. Every generated walk is driven with real clicks and
// keys against a real serve (llm-control is the model), and at every
// node readUiState must equal the spec's Thread#0 state, read off the
// DOM only, and the screen owes the shared invariants: no sideways
// scroll, the status on screen, no console error, no clipped header or
// status text, a ring on whatever holds keyboard focus, and nothing axe
// finds on the thread. Recipe: go/tests/model/README.md.
//
// The server's steps are held so each node can be looked at:
//
//   - the transcript's first read (GET /api/sessions/<id>) is held at
//     the network until Loaded, or answered with a body that is not JSON
//     for LoadFails (a 5xx would log a console error of its own);
//   - a send (POST …/prompt) is held until Take: that is the "Sending…"
//     row the spec's `sending` names;
//   - a turn is an llm-control "block" turn; Delta streams a fragment
//     into it, CallStart releases it as one bash call that waits on a
//     gate file outside the checkout, CallOk / CallFails open the gate
//     (the call writes out.txt, or exits 1), and the model's next
//     request is another block turn that Finish or Fail releases.
//
// On a sub page the transcript is not on screen: the fields only the
// thread shows (the transcript read, the status, the turn's rows) are
// the ones read the last time the thread was, which is what the spec
// says of them too (a sub page changes none of them).
import * as fs from 'fs';
import * as path from 'path';
import { execFileSync } from 'child_process';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';
import { uiInvariants } from '../../helpers/ui-invariants';

// Up to 35 steps a walk, each with an axe pass.
test.describe.configure({ timeout: 240_000 });

const WIDE = { width: 1100, height: 700 };
const PHONE = { width: 600, height: 700 };

// A brief longer than the bubble's four-line clamp: shown in full (its
// "Show full prompt") it is taller than the pane, which ScrollUp needs.
const PROMPT = Array.from({ length: 30 }, (_, i) =>
  `Step ${i + 1}: split the parser into modules, keep the public API exactly as it is, add a test for each module, and report what moved where.`).join(' ');

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;       // the session's git checkout
  q: string;         // llm-control's dir
  gates: string;     // where the call's gate files live (outside the checkout)
  n: number;         // turns started
  held: string;      // the llm-control block turn in flight, '' when none
  gate: string;      // the running call's gate file, '' when none
  streamed: string;  // what Delta streamed into the held turn, not recorded yet
  holdLoad: boolean; // hold transcript reads until Loaded
  loads: Route[];    // transcript reads held
  posts: Route[];    // sends held
  last: Record<string, unknown>; // the thread's fields as last read on the thread
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until(what: string, ok: () => boolean, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!ok()) {
    if (Date.now() > deadline) throw new Error(`ui_thread: ${what}`);
    await sleep(20);
  }
}

function git(c: Ctx, ...args: string[]): void {
  execFileSync('git', ['-C', c.dir, '-c', 'user.name=model', '-c', 'user.email=model@test', '-c', 'commit.gpgsign=false', ...args], {
    env: { ...process.env, HOME: c.serve.home, GIT_CONFIG_NOSYSTEM: '1' },
    stdio: 'pipe',
  });
}

// --- readUiState

const HEAD_WORDS: Record<string, string> = { Running: 'running', Done: 'done', Failed: 'error', Idle: 'idle', Stopped: 'stopped' };

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await c.page.evaluate((id) => {
    const hash = location.hash;
    const route = hash.startsWith(`#/s/${id}/changes`) ? 'changes' : hash.startsWith(`#/s/${id}/context`) ? 'context' : hash === `#/s/${id}` ? 'thread' : `other: ${hash}`;
    const a = document.activeElement as HTMLElement | null;
    const pop = document.querySelector('.thread-head .head-pop[role="dialog"]');
    const focus = pop && a && pop.contains(a) ? 'settings'
      : a && (a.matches('.thread-head button.more') || a.matches('.thread-head .runtime-strip details > summary')) ? 'trigger' : 'other';
    const chip = document.querySelector<HTMLDetailsElement>('.thread-head details.rt-jobs:has(a.chg-full)');
    const more = document.querySelector<HTMLDetailsElement>('.thread-head details.rt-more');
    const base = { wide: window.innerWidth > 720, route, settings: !!pop, chg: !!chip?.open, details: !!more?.open, focus };
    const tr = document.querySelector('.thread .scroll.transcript');
    if (route !== 'thread' || !tr) return { ...base, thread: null };
    // The first read: its error note, its loading note, or neither.
    const failedNote = [...tr.querySelectorAll('.transcript-state')].some((e) => /Couldn’t load this session|taking too long/.test(e.textContent ?? ''));
    const loadingNote = [...tr.querySelectorAll('.transcript-state')].some((e) => /Loading transcript/.test(e.textContent ?? ''));
    const main = document.querySelector('.thread-head .head-main');
    const live = main?.querySelector('.head-live');
    const failedHead = main?.querySelector('.head-failed');
    const mark = main?.querySelector(':scope > .status');
    // The words a sighted reader sees: a live header's mark carries a
    // hidden "Running" of its own.
    const seen = (e: Element) => [...e.childNodes].filter((n) => !(n instanceof HTMLElement && n.matches('.visually-hidden, .status:has(.visually-hidden)'))).map((n) => n.textContent ?? '').join('').trim();
    const head = live ? `live:${seen(live)}` : failedHead ? 'failed-turn' : mark ? `mark:${seen(mark) || (mark.textContent ?? '').trim()}` : '';
    // The latest turn's call: a retried turn makes none, and the one
    // before it keeps its own row above. A send not taken yet is no turn
    // (its copy says "Sending…"); once taken it is, recorded or not.
    const turns = tr.querySelectorAll('section.turn:not(.turn-sending)');
    const call = turns[turns.length - 1]?.querySelector<HTMLDetailsElement>('details.call-native') ?? null;
    const callState = !call ? 'none' : call.querySelector(':scope > summary .tool-running') ? 'running' : call.classList.contains('block-failed') ? 'failed' : 'ok';
    const work = tr.querySelector<HTMLDetailsElement>('details.work-seg-live');
    const jump = [...document.querySelectorAll<HTMLButtonElement>('.composer-actions button.jump-latest')].find((b) => /jump to latest/i.test(b.getAttribute('aria-label') ?? ''));
    return {
      ...base,
      thread: {
        failedNote, loadingNote, head,
        empty: !!tr.querySelector('.thread-empty'),
        sending: !!tr.querySelector('.turn-sending-state'),
        streamed: !!tr.querySelector('.stream-say'),
        call: callState,
        callOpen: !!call?.open,
        workOpen: !!work?.open,
        away: !!jump,
        fresh: !!jump && /^New activity/.test(jump.getAttribute('aria-label') ?? ''),
        usage: !!document.querySelector('.thread-head .runtime-strip button.rt-link[aria-label^="Context"]'),
        edits: !!tr.querySelector('.turn-files'),
        errCard: !!tr.querySelector('.err-card'),
      },
    };
  }, c.id);

  let fields: Record<string, unknown>;
  if (dom.thread) {
    const t = dom.thread;
    const transcript = t.failedNote ? 'failed' : t.loadingNote || t.head === '' ? 'loading' : 'ok';
    // The header's word, as the session's status: "Sending" is a send
    // on its way over an idle session (the first turn is the only one
    // the composer starts); a turn that ended on a failed call reads
    // "Failed" beside a done session.
    let status: string;
    if (transcript !== 'ok') status = String(c.last.status ?? 'idle');
    // A live header over a send not yet recorded is that send ("Sending",
    // or "Waiting" once accepted); otherwise the turn it started runs.
    else if (t.head.startsWith('live:')) status = t.sending ? 'idle' : 'running';
    else if (t.head === 'failed-turn') status = 'done';
    else if (t.head.startsWith('mark:')) status = HEAD_WORDS[t.head.slice(5)] ?? `unknown: ${t.head}`;
    else status = `unknown: ${t.head}`;
    fields = {
      transcript, status,
      sending: t.sending, streamed: t.streamed, call: t.call, callOpen: t.callOpen, workOpen: t.workOpen,
      away: t.away, fresh: t.fresh, usage: t.usage, edits: t.edits, errCard: t.errCard,
    };
    c.last = fields;
  } else {
    fields = c.last;
  }
  return {
    viewport: dom.wide ? 'wide' : 'phone',
    route: dom.route,
    ...fields,
    settings: dom.settings,
    chg: dom.chg,
    details: dom.details,
    focus: dom.focus,
  };
}

// --- the server's side

// Continue every held transcript read, and let later ones through.
async function answerLoads(c: Ctx, fail: boolean): Promise<void> {
  await until('no transcript read to answer', () => c.loads.length > 0);
  if (!fail) c.holdLoad = false;
  for (const r of c.loads.splice(0)) {
    if (fail) r.fulfill({ status: 200, contentType: 'application/json', body: 'not json' }).catch(() => {});
    else r.continue().catch(() => {});
  }
}

function turnName(c: Ctx, part: string): string {
  return `t${String(c.n).padStart(4, '0')}${part}`;
}

// The bash call CallStart puts in the turn: it waits for its gate, then
// writes a file (ok) or fails without writing one.
function callCommand(gate: string): string {
  return `while [ ! -e '${gate}' ]; do sleep 0.05; done; if [ "$(cat '${gate}')" = ok ]; then printf 'moved\\n' > out.txt; else echo 'no such module' >&2; exit 1; fi`;
}

function openGate(c: Ctx, word: string): void {
  if (!c.gate) throw new Error('ui_thread: no call running');
  fs.writeFileSync(c.gate + '-tmp', word);
  fs.renameSync(c.gate + '-tmp', c.gate);
  c.gate = '';
}

// --- the page

const head = (c: Ctx) => c.page.locator('.thread-head');
const transcript = (c: Ctx) => c.page.locator('.thread .scroll.transcript');
const chipSummary = (c: Ctx) => head(c).locator('details.rt-jobs:has(a.chg-full) > summary');
const moreSummary = (c: Ctx) => head(c).locator('details.rt-more > summary');
const contextChip = (c: Ctx) => head(c).locator('.runtime-strip button.rt-link[aria-label^="Context"]');

async function thePops(c: Ctx): Promise<{ settings: boolean; chg: boolean; details: boolean }> {
  const s = await readUiState(c);
  return { settings: Boolean(s.settings), chg: Boolean(s.chg), details: Boolean(s.details) };
}

modelTests<Ctx>({
  spec: 'ui_thread',
  role: 'Thread#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const dir = fs.mkdtempSync(path.join(serve.work, 'thread-'));
    const c: Ctx = {
      page, serve, id: '', dir, q: controlDir(serve.home), gates: fs.mkdtempSync(path.join(serve.home, 'gates-')),
      n: 0, held: '', gate: '', streamed: '', holdLoad: true, loads: [], posts: [], last: {},
    };
    git(c, 'init', '-q', '-b', 'main');
    fs.writeFileSync(path.join(dir, 'README'), 'thread\n');
    git(c, 'add', 'README');
    git(c, 'commit', '-q', '-m', 'base');
    const res = await serve.api.post('/api/sessions', { data: { cwd: dir, prompt: '' } });
    if (!res.ok()) throw new Error(`create session: ${res.status()} ${await res.text()}`);
    c.id = (await res.json()).session.id;

    await page.route((u) => u.pathname === `/api/sessions/${c.id}` && !u.searchParams.has('since'), (r) => {
      if (c.holdLoad && r.request().method() === 'GET') c.loads.push(r);
      else r.continue().catch(() => {});
    });
    await page.route((u) => u.pathname === `/api/sessions/${c.id}/prompt`, (r) => { c.posts.push(r); });
    await page.setViewportSize(WIDE);
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await until('the first transcript read never came', () => c.loads.length > 0);
    return c;
  },

  actions: {
    Loaded: (c) => answerLoads(c, false),
    LoadFails: (c) => answerLoads(c, true),
    RetryLoad: (c) => transcript(c).locator('.transcript-state').getByRole('button', { name: 'Retry', exact: true }).click(),

    async Send(c) {
      await c.page.locator('#composer').fill(PROMPT);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
    },
    // The latest card's: an earlier failure keeps its own card above.
    RetryTurn: (c) => transcript(c).locator('.err-card').last().getByRole('button', { name: 'Retry', exact: true }).click(),

    ToggleCall: (c) => transcript(c).locator('section.turn:not(.turn-sending)').last().locator('details.call-native > summary').click(),
    ToggleWork: (c) => transcript(c).locator('details.work-seg-live > summary').click(),
    // The wheel over the transcript, as a person scrolls up. A single
    // running turn fits the pane with its brief clamped, so the person
    // first opens the brief in full (a disclosure the spec does not
    // model: it moves no field), reads down to the end, and then scrolls
    // back up through it.
    async ScrollUp(c) {
      const box = await transcript(c).boundingBox();
      if (!box) throw new Error('ScrollUp: no transcript on screen');
      const more = transcript(c).getByRole('button', { name: 'Show full prompt' });
      if (await more.count()) {
        await more.first().click();
        await c.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
        await c.page.mouse.wheel(0, 4000);
        await expect.poll(() => transcript(c).evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight), { message: 'ScrollUp: never reached the end' }).toBeLessThan(2);
      }
      await c.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await c.page.mouse.wheel(0, -2000);
      const dims = await transcript(c).evaluate((el) => `${el.scrollHeight} tall in ${el.clientHeight}, at ${el.scrollTop}`);
      await expect.poll(() => transcript(c).evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight), { message: `ScrollUp: the transcript did not move (${dims})` }).toBeGreaterThanOrEqual(40);
    },
    JumpLatest: (c) => c.page.locator('.composer-actions button.jump-latest[aria-label$="jump to latest"], .composer-actions button.jump-latest[aria-label="Jump to latest"]').click(),

    OpenSettings: (c) => head(c).getByRole('button', { name: 'Session settings' }).click(),
    OpenChanges: (c) => chipSummary(c).click(),
    OpenDetails: (c) => moreSummary(c).click(),
    Escape: (c) => c.page.keyboard.press('Escape'),
    // A click on the transcript's empty space, clear of every row.
    async ClickAway(c) {
      const box = await transcript(c).boundingBox();
      if (!box) throw new Error('ClickAway: no transcript on screen');
      await c.page.mouse.click(box.x + 6, box.y + box.height - 6);
    },
    async OpenFullChanges(c) {
      const p = await thePops(c);
      if (p.chg) await head(c).locator('details.rt-jobs a.chg-full').click();
      else if (p.details) await head(c).locator('details.rt-more a.rt-link[href$="/changes"]').click();
      else await transcript(c).locator('.turn-files').click();
    },
    OpenContext: (c) => contextChip(c).click(),
    Back: (c) => c.page.keyboard.press('Escape'),
    async Resize(c) {
      const wide = (c.page.viewportSize()?.width ?? 0) > 720;
      await c.page.setViewportSize(wide ? PHONE : WIDE);
    },

    // --- the server
    async Take(c) {
      c.n++;
      c.held = turnName(c, 'a');
      c.streamed = '';
      queue(c.q, c.held, { mode: 'block', text: `finished turn ${c.n}` });
      await until('no send to take', () => c.posts.length > 0);
      for (const r of c.posts.splice(0)) r.continue().catch(() => {});
      await waitTaken(c.q, c.held);
    },
    async Delta(c) {
      const f = path.join(c.q, c.held + '.stream');
      c.streamed = 'Reading the parser first. ';
      fs.writeFileSync(f + '-tmp', c.streamed);
      fs.renameSync(f + '-tmp', f);
      await until('the delta was not streamed', () => !fs.existsSync(f));
    },
    async CallStart(c) {
      const next = turnName(c, 'b');
      queue(c.q, next, { mode: 'block', text: `finished turn ${c.n}` });
      c.gate = path.join(c.gates, `${c.n}.gate`);
      // A provider's response carries the text it streamed before the
      // call, so what Delta streamed is recorded with the call.
      releaseWith(c.q, c.held, { text: c.streamed, call: { name: 'bash', args: { command: callCommand(c.gate) } } } as never);
      c.streamed = '';
      c.held = next;
    },
    async CallOk(c) {
      openGate(c, 'ok');
      await waitTaken(c.q, c.held);
    },
    async CallFails(c) {
      openGate(c, 'no');
      await waitTaken(c.q, c.held);
    },
    async Finish(c) {
      release(c.q, c.held);
      c.held = '';
    },
    async Fail(c) {
      releaseWith(c.q, c.held, { mode: 'error', error: 'model says no' });
      c.held = '';
    },
  },

  read: readUiState,
  status: (c) => c.page.locator('.thread-head h1').first(),
  // This graph describes UI state, which is checked in the browser.
  sessions: () => [],
  async check(c, where) {
    // Judged at rest: a hover's fade (the prompt's time and Copy under
    // the pointer) runs on real time, and axe measuring halfway through
    // one reports a contrast the screen never settles on.
    await Promise.race([sleep(2000), c.page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
      .map((a) => a.finished.catch(() => {}))))]);
    // The title is the first prompt, a paragraph here: the header cuts it
    // with an ellipsis on purpose, and then owes the whole of it as the
    // heading's tooltip. Everything else in the header is never cut.
    const title = await c.page.locator('.thread-head h1').first().evaluate((h) =>
      h.scrollWidth <= h.clientWidth + 1 || (h.getAttribute('title') ?? '').trim().startsWith((h.textContent ?? '').trim().replace(/…$/, '')) ? '' : `"${h.textContent}" is cut and its tooltip is "${h.getAttribute('title')}"`);
    expect(title, `${where}: clipped title`).toBe('');
    await uiInvariants(c.page, {
    include: ['.thread-head', '.thread .scroll.transcript', '.composer-actions', '.chg-page'],
    text: '.thread-head .head-main > .status, .thread-head .head-live, .jump-word, .err-head, p.working, .work-seg > summary .block-label, .call-native > summary .block-label, .turn-files, .turn-sending-state, .thread-empty h2',
    }, where);
  },
  async cleanup(c) {
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
    if (c.gate) openGate(c, 'no');
    if (c.held) release(c.q, c.held);
  },
});
