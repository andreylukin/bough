// go/tests/model/specs/native_call_adoption.fizz in the browser (recipe:
// go/tests/model/README.md): an engine call that outlives turn_settle is
// adopted as a job, its turn closes with done{running: 1}, and the call
// then ends on its own, is killed from Work, or loses its child.
//
// Every walk runs on a serve of its own (engine-unreal, llm-control as
// the model, turn_settle SETTLE) through one thread's page. The long call
// is a bash call waiting on a gate file, so the walk decides when it
// ends. Like steer-queue.spec.ts it walks with its own loop rather than
// helpers/model.ts's modelTests, for three things this flow needs:
//
// - The spec splits what the product does in one go. A steer is sent
//   (Steer) and then taken (SteerLands), a kill is sent (KillJob) and then
//   acted on (KillLands): the page's POST is held at the route until the
//   step that lands it, a slow network and nothing more. An end while
//   idle is recorded (CallEnds) and then wakes the model (WakeRequest):
//   the product wakes at once, so WakeRequest is one of the automatic
//   steps a read may already have taken.
// - The page catches up on its own (CatchUp is the page's GET, not the
//   person's). The page is compared with the spec's state as a CatchUp
//   would leave it: Work's word is the job's record, and the footer's
//   count is done.running less the calls reported since. A page that
//   lags is polled until it catches up; one that never does fails.
// - Where the spec forks and the product picks (a prompt that beats the
//   wake), the walk ends there as "not taken" when the page shows a state
//   the spec allows after that step but the path did not take, as the Go
//   adapter's fmbt.ErrNotImplemented does.
//
// What the page shows, and so what is compared: the row's status, the
// open turn (a user's or a wake's), "Steer pending…", Work's word for the
// job, the row's job count (Row.jobs) and the footer's "N calls still
// running". The rest of the role (the call's end row, finishes, the
// model's request, the child's generation) is history's and is checked
// by the Go walks (mbt/native_call_adoption_test.go) and the trace check.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, releaseWith, type Turn } from '../../helpers/control';
import { loadPaths, roleState, type Step, walkTest } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';
import { loadGraph, walks } from '../../model/graph';

const SPEC = 'native_call_adoption';
const ROLE = 'Session#0';
type State = Record<string, unknown>;

// Long enough that a steer the route holds (Steer, then maybe CallEnds)
// still lands inside the window on a loaded machine.
const SETTLE_MS = 8_000;
const CONFIG = CONTROL_CONFIG + `- id: loop\n  plugin: engine-unreal\n  config:\n    turn_settle: ${SETTLE_MS / 1000}s\n`;
test.use({ serveOpts: { config: CONFIG } });

// ---- the graph: every link, whatever MODEL_COVER walks ----

const ENDED = new Set(['finished', 'stopped']);
// The system's and the page's own steps: nothing the person does.
const AUTOMATIC = new Set(['CatchUp', 'WakeRequest']);

const bare = (action: string) => (action.startsWith(ROLE + '.') ? action.slice(ROLE.length + 1) : action);
const key = (s: State) => JSON.stringify(Object.keys(s).sort().map((k) => [k, s[k]]));

// The page's view of a spec state: as a CatchUp would leave it.
function view(s: State): State {
  const job = s.job as string;
  return {
    status: s.status,
    turn: s.turn,
    steer_q: s.steer_q,
    work: job,
    listed: s.listed,
    still: ENDED.has(job) ? 0 : s.foot_running,
  };
}
const seen = (s: State) => key(view(s));

const all = walks(loadGraph(path.resolve(__dirname, '..', '..', '..', 'model', 'testdata', SPEC)), 'transitions').paths.map((p) => p.trace);
const graph = new Map<string, { action: string; to: State }[]>();
for (const trace of all) {
  for (let i = 1; i < trace.length; i++) {
    const from = roleState(ROLE, trace[i - 1].state), to = roleState(ROLE, trace[i].state), action = bare(trace[i].action);
    const out = graph.get(key(from)) ?? [];
    if (!out.some((e) => e.action === action && key(e.to) === key(to))) out.push({ action, to });
    graph.set(key(from), out);
  }
}

// Every view the spec allows once `action` is done in `from`: its
// successors, and whatever the automatic steps make of them.
function allowedAfter(from: State, action: string): Set<string> {
  const out = new Set<string>();
  const todo = (graph.get(key(from)) ?? []).filter((e) => e.action === action).map((e) => e.to);
  const done = new Set<string>();
  while (todo.length) {
    const s = todo.pop()!;
    if (done.has(key(s))) continue;
    done.add(key(s));
    out.add(seen(s));
    for (const e of graph.get(key(s)) ?? []) if (AUTOMATIC.has(e.action)) todo.push(e.to);
  }
  return out;
}

// ---- the walk's hands ----

interface Entry { seq: number; kind: string; data?: Record<string, unknown>; text?: string }

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;       // llm-control's queue
  id: string;
  gate: string;      // the long call exits once this exists
  started: string;   // the long call writes this as it starts
  callId: string;
  turns: number;
  steer?: Route;     // the steer's POST, held until it lands
  kill?: Route;      // the kill's POST, held until the child acts on it
  passPrompt: boolean;
  // ChildExits dropped the held kill: Chromium logs the aborted request.
  abortedKill: boolean;
}

// A step the product took the other branch of; the walk ends there.
class NotTaken extends Error {}

function entries(c: Ctx): Entry[] {
  const f = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  if (!fs.existsSync(f)) return [];
  const out: Entry[] = [];
  for (const l of fs.readFileSync(f, 'utf8').split('\n')) {
    if (!l.trim()) continue;
    try { out.push(JSON.parse(l)); } catch { /* a line still being written */ }
  }
  return out;
}
const lastSeq = (c: Ctx) => entries(c).reduce((n, e) => Math.max(n, e.seq ?? 0), 0);
const kinds = (c: Ctx) => entries(c).filter((e) => !['meta', 'usage', 'title', 'summary'].includes(e.kind))
  .map((e) => `${e.seq}:${e.kind}${e.kind === 'job' ? `(${e.data?.event} ${e.data?.id})` : ''}`).join(' ');

async function until(c: Ctx, what: string, ok: () => boolean | Promise<boolean>, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`${SPEC}: waiting for ${what}: transcript ${kinds(c)}`);
    await new Promise((r) => setTimeout(r, 30));
  }
}

const isWake = (e: Entry) => e.kind === 'input' && e.data?.wake === true;
const typedJob = (e: Entry) => e.kind === 'job' && typeof e.data?.event === 'string';
function jobState(c: Ctx): string {
  let st = 'none';
  for (const e of entries(c)) {
    if (!typedJob(e) || e.data?.call !== c.callId || !c.callId) continue;
    if (e.data?.event === 'started') st = 'running';
    if (e.data?.event === 'finished') st = e.data?.stopped === true ? 'stopped' : 'finished';
  }
  return st;
}
const endRow = (c: Ctx) => entries(c).find((e) => e.kind === 'call' && c.callId !== '' && e.data?.id === c.callId);

// Two block turns always queued: every model request is held until the
// walk answers it.
function topUp(c: Ctx): void {
  const queued = fs.readdirSync(c.dir).filter((f) => f.endsWith('.json')).length;
  for (let n = queued; n < 2; n++) queue(c.dir, `q${String(++c.turns).padStart(6, '0')}`, { mode: 'block', text: 'answered' });
}
function held(c: Ctx): string[] {
  return fs.readdirSync(c.dir).filter((f) => f.endsWith('.taken')).map((f) => f.slice(0, -'.taken'.length))
    .filter((n) => !fs.existsSync(path.join(c.dir, n + '.release')));
}
function answer(c: Ctx, turn: Turn): string[] {
  const names = held(c);
  for (const n of names) releaseWith(c.dir, n, turn);
  topUp(c);
  return names;
}
const waitHeld = (c: Ctx, what: string) => until(c, what, () => held(c).length > 0);

const composer = (c: Ctx) => c.page.locator('#composer');
const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();

// The composer, the way a person sends: the POST goes through.
async function prompt(c: Ctx, text: string): Promise<void> {
  topUp(c);
  const since = lastSeq(c);
  c.passPrompt = true;
  await composer(c).fill(text);
  await composer(c).press('Enter');
  await until(c, 'the prompt\'s input', () => entries(c).some((e) => e.seq > since && e.kind === 'input' && !e.data?.steer && String(e.data?.text ?? '').trim() === text));
  c.passPrompt = false;
  await waitHeld(c, 'the prompt\'s model request');
}

// Lets the held steer reach serve and waits for the engine to record it.
async function landSteer(c: Ctx): Promise<void> {
  const r = c.steer;
  if (!r) throw new Error('no steer held');
  c.steer = undefined;
  await r.continue();
  await until(c, 'the steer\'s input', () => entries(c).some((e) => e.kind === 'input' && e.data?.steer === true));
}

// The adopted call's end row and the job's finish are recorded; an end
// while idle wakes the model at once (the product's WakeRequest).
async function endAdopted(c: Ctx, idle: boolean): Promise<void> {
  await until(c, 'the adopted call\'s end row and finish', () => endRow(c) !== undefined && jobState(c) !== 'running');
  if (idle) await until(c, 'the wake input', () => entries(c).some(isWake));
}

type Act = (c: Ctx, before: State, after: State) => Promise<void>;

const actions: Record<string, Act> = {
  Prompt: (c) => prompt(c, 'run the long build'),
  // Enter while the turn is live: a steer, held at the route.
  async Steer(c) {
    await composer(c).fill('also check the logs');
    await composer(c).press('Enter');
    await until(c, 'the steer to reach the network', () => c.steer !== undefined);
  },
  async PromptWhileAdopted(c, before) {
    // The coordinator asks the model the moment the end is recorded; a
    // prompt only beats it when the wake is not out yet.
    if (before.wake_pending && entries(c).some(isWake)) throw new NotTaken('the wake opened before the prompt');
    await prompt(c, 'meanwhile a question');
  },
  // Work's Stop on the job: the POST is held until KillLands.
  async KillJob(c) {
    await c.page.locator('button.work-summary').click();
    const dialog = c.page.getByRole('dialog');
    await dialog.getByRole('button', { name: /^Stop Job \d+$/ }).click();
    await until(c, 'the kill to reach the network', () => c.kill !== undefined);
    await c.page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
  },
  async PromptAfterExit(c, before) {
    await prompt(c, 'are you still there?');
    if (before.call === 'lost') {
      await until(c, 'the lost call\'s interrupted end', () => String(endRow(c)?.data?.error ?? '').startsWith('interrupted'));
    }
  },
  // The first request answers with the long bash call.
  async CallsBash(c) {
    const cmd = `touch ${c.started}; while [ ! -e ${c.gate} ]; do sleep 0.05; done; echo built`;
    const names = answer(c, { bash: cmd } as unknown as Turn);
    if (names.length !== 1) throw new Error(`CallsBash: want one request in flight, have ${names}`);
    c.callId = 'control_' + names[0];
    await until(c, 'the long call to start', () => fs.existsSync(c.started));
  },
  async Reply(c, before) {
    const since = lastSeq(c);
    if (before.turn === 'user' && before.call === 'fg') {
      // The turn stays open on its call; nothing asks the model again.
      if (!answer(c, { mode: 'ok', text: 'reply' }).length) throw new Error('no model request in flight to answer');
      await until(c, 'the reply\'s assistant entry', () => entries(c).some((e) => e.seq > since && e.kind === 'assistant'));
      await new Promise((r) => setTimeout(r, 300));
      if (held(c).length) throw new Error(`the model was asked again (${held(c)}) while the turn should wait on its call`);
      return;
    }
    if (before.steer_q) {
      // The steer reaches serve before the answer, so it lands in this turn.
      await landSteer(c);
      answer(c, { mode: 'ok', text: 'reply' });
      await waitHeld(c, 'the model to be asked with the steer');
      return;
    }
    await until(c, 'the turn\'s done', () => {
      answer(c, { mode: 'ok', text: 'reply' });
      return entries(c).some((e) => e.seq > since && e.kind === 'done');
    });
  },
  async SteerLands(c) {
    await landSteer(c);
    await waitHeld(c, 'the model to be asked with the steer');
  },
  async SettleFires(c, before) {
    if (before.steer_q) return actions.SteerLands(c, before, before);
    const since = lastSeq(c);
    await until(c, 'the call\'s adoption and its turn\'s done',
      () => jobState(c) !== 'none' && entries(c).some((e) => e.seq > since && e.kind === 'done'), SETTLE_MS + 15_000);
  },
  async CallEnds(c, before) {
    fs.writeFileSync(c.gate, '');
    if (before.call === 'fg') {
      await until(c, 'the call\'s end row', () => endRow(c) !== undefined);
      await waitHeld(c, 'the model to be asked with the result');
      return;
    }
    await endAdopted(c, before.turn === 'none');
  },
  // The child acts on the held /jobkill.
  async KillLands(c, before) {
    const r = c.kill;
    c.kill = undefined;
    if (r) await r.continue();
    if (before.job === 'running') await endAdopted(c, before.turn === 'none');
  },
  async WakeRequest(c) { await until(c, 'the wake input', () => entries(c).some(isWake)); },
  // A crash: SIGKILL the session's child (the one whose cwd is the walk's).
  async ChildExits(c) {
    // The spec drops a kill the dead child never acted on; the page's POST
    // is cut off like a connection that died with it.
    if (c.kill) { c.abortedKill = true; await c.kill.abort().catch(() => {}); c.kill = undefined; }
    const cwd = fs.realpathSync(c.serve.work);
    const out = execFileSync('lsof', ['-a', '-d', 'cwd', '-c', 'bough', '-Fpn'], { encoding: 'utf8' });
    let pid = 0, killed = false;
    for (const l of out.split('\n')) {
      if (l.startsWith('p')) pid = Number(l.slice(1));
      else if (l.startsWith('n') && l.slice(1) === cwd && pid > 0) { process.kill(pid, 'SIGKILL'); killed = true; }
    }
    if (!killed) throw new Error(`no bough child in ${cwd} to kill`);
    // Its orphaned command is nobody's now; let it go.
    fs.writeFileSync(c.gate, '');
    await until(c, 'serve to record the job stopped', () => jobState(c) === 'stopped');
  },
  async CatchUp() { /* the page's own GET */ },
  async end() { /* a quiet thread with its bounds used up */ },
};

// ---- readUiState: the page's view, from the DOM only ----

async function read(c: Ctx): Promise<State> {
  const dom = await c.page.evaluate((id) => {
    const label = document.querySelector(`button.row[data-id="${id}"]`)?.getAttribute('aria-label') ?? '';
    const jobs = document.querySelector(`button.row[data-id="${id}"] .row-jobs`)?.textContent ?? '';
    const turns = [...document.querySelectorAll('section.turn[data-turn]')];
    const last = turns.at(-1);
    const work = document.querySelector('button.work-summary')?.getAttribute('aria-label') ?? null;
    const still = [...document.querySelectorAll('.turn-foot .turn-running')].map((e) => e.textContent ?? '');
    const pending = [...document.querySelectorAll('.turn-sending-state')].map((e) => e.textContent ?? '');
    return { label, jobs, wake: Boolean(last?.querySelector('.call-wake')), work, still, pending };
  }, c.id);
  const running = /, Running, /.test(dom.label);
  const count = (re: RegExp) => Number(re.exec(dom.work ?? '')?.[1] ?? 0);
  const work = dom.work === null ? 'none'
    : count(/\b(\d+) running\b/) === 1 ? 'running'
    : count(/\b(\d+) finished\b/) === 1 ? 'finished'
    : count(/\b(\d+) stopped\b/) === 1 ? 'stopped'
    : `unknown: ${dom.work}`;
  return {
    status: running ? 'running' : 'idle',
    turn: running ? (dom.wake ? 'wake' : 'user') : 'none',
    steer_q: dom.pending.some((p) => p.startsWith('Steer pending')),
    work,
    listed: /\d/.test(dom.jobs) && Number(/\d+/.exec(dom.jobs)![0]) > 0,
    still: dom.still.reduce((n, t) => n + Number(/^(\d+) calls? still running$/.exec(t.trim())?.[1] ?? NaN), 0),
  };
}

// Every node: no sideways scroll, the row on screen, nothing logged as
// an error, and the spec's page-side claims about what it shows.
async function invariants(c: Ctx, got: State, errors: string[], where: string): Promise<void> {
  const overflow = await c.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(row(c), `${where}: status not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
  // FinishedNotListed: the row never counts a job Work says ended.
  expect(got.listed && ENDED.has(got.work as string), `${where}: an ended job is listed`).toBe(false);
  // FooterBounded: the footer never counts an ended job as running.
  expect(ENDED.has(got.work as string) && (got.still as number) > 0, `${where}: footer says an ended call still runs`).toBe(false);
}

// The page's timers run on a clock the walk moves: each read is a few
// page-seconds after the last, so the list poll runs without real waits.
const POLL_STEP_MS = 5_000;

// Waits for the page to show node n, or a later one reached by automatic
// steps only. Returns the node it matched, or -1 when the page settled on
// a view the spec allows after the step but the path did not take.
async function settle(c: Ctx, trace: Step[], n: number, where: string, errors: string[]): Promise<{ m: number; got: State }> {
  const want: string[] = [];
  for (let m = n; m < trace.length; m++) {
    if (m > n && !AUTOMATIC.has(bare(trace[m].action))) break;
    want.push(seen(roleState(ROLE, trace[m].state)));
  }
  const allowed = n > 0 ? allowedAfter(roleState(ROLE, trace[n - 1].state), bare(trace[n].action)) : new Set<string>();
  const deadline = Date.now() + 15_000;
  let last: State = {};
  for (;;) {
    await c.page.clock.fastForward(POLL_STEP_MS);
    last = await read(c);
    const i = want.indexOf(key(last));
    if (i >= 0) return { m: n + i, got: last };
    if (Date.now() > deadline) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  if (allowed.has(key(last))) return { m: -1, got: last };
  expect(last, `${where}: state${errors.length ? ` (console: ${errors.join(' | ')})` : ''}\ntranscript ${kinds(c)}`)
    .toEqual(view(roleState(ROLE, trace[n].state)));
  return { m: n, got: last };
}

function saveTranscript(c: Ctx, title: string): void {
  const root = process.env.MODEL_TRACE_DIR;
  if (!root || !c.id) return;
  const dir = path.join(root, SPEC);
  fs.mkdirSync(dir, { recursive: true });
  const src = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  const slug = title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 120);
  if (fs.existsSync(src)) fs.copyFileSync(src, path.join(dir, `${slug}-${c.id}.jsonl`));
}

test.describe(`model: ${SPEC}`, () => {
  loadPaths(SPEC).forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => bare(s.action)).join(' → ');
    walkTest(SPEC)(`path ${i}: ${walk}`, async ({ serve, page }, info) => {
      test.setTimeout(120_000);
      const c: Ctx = {
        page, serve, dir: controlDir(serve.home), id: '', turns: 0, callId: '', passPrompt: false, abortedKill: false,
        gate: path.join(serve.home, 'gate'), started: path.join(serve.home, 'started'),
      };
      const errors: string[] = [];
      page.on('console', (m) => {
        if (m.type() !== 'error') return;
        if (c.abortedKill && m.text() === 'Failed to load resource: net::ERR_FAILED') { c.abortedKill = false; return; }
        errors.push(m.text());
      });
      page.on('pageerror', (e) => errors.push(String(e.stack ?? e)));
      fs.mkdirSync(c.dir, { recursive: true });
      topUp(c);
      // Init: an idle thread with a live child, open on the page.
      c.id = await serve.newSession('');
      await until(c, 'the idle session\'s child', async () => {
        const r = await serve.api.get(`/api/sessions/${c.id}`);
        return r.ok() && (await r.json()).session.live === true;
      });
      await page.route(`**/api/sessions/${c.id}/prompt`, (route) => {
        if (c.passPrompt) return route.continue();
        c.steer = route;
      });
      await page.route(`**/api/sessions/${c.id}/jobs/*/kill`, (route) => { c.kill = route; });
      await page.clock.install();
      await page.goto(`${serve.url}/#/s/${c.id}`);

      let reached = 0;
      try {
        for (let n = 0; n < trace.length; n++) {
          const name = n === 0 ? 'Init' : bare(trace[n].action);
          const where = `step ${n} (${name})`;
          if (n > 0 && n <= reached) continue; // an automatic step the page already took
          if (n > 0) {
            const act = actions[name];
            if (!act) throw new Error(`${SPEC}: no action for ${trace[n].action}`);
            try {
              await act(c, roleState(ROLE, trace[n - 1].state), roleState(ROLE, trace[n].state));
            } catch (e) {
              if (!(e instanceof NotTaken)) throw e;
              info.annotations.push({ type: 'not-taken', description: `${where}: ${e.message}` });
              break;
            }
          }
          const { m, got } = await settle(c, trace, n, where, errors);
          if (m < 0) {
            info.annotations.push({ type: 'not-taken', description: `${where}: the page took another branch the spec allows` });
            break;
          }
          await invariants(c, got, errors, where);
          reached = m;
        }
      } finally {
        info.annotations.push({ type: 'reached', description: String(reached) });
        await page.goto('about:blank');
        for (const r of [c.steer, c.kill]) await r?.abort().catch(() => {});
        fs.writeFileSync(c.gate, '');
        saveTranscript(c, info.title);
        answer(c, { mode: 'ok', text: 'reply' });
      }
    });
  });
});
