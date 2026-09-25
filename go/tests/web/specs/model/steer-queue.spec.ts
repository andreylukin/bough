// Model-based browser walk of go/tests/model/specs/steer_queue.fizz:
// Enter steers a running turn, Cmd/Ctrl+Enter queues behind it, and Stop
// may swallow a line already written (recipe: go/tests/model/README.md).
//
// Every generated path is walked through one thread's page against the
// worker's serve (llm-control as the model), and at every node the state
// read off the DOM must be the spec's. It walks with its own loop rather
// than helpers/model.ts's modelTests, for three things this flow needs:
//
// - The network is part of the walk. Every POST /prompt the page makes is
//   held at the route until the step that lands it (Start for a message,
//   SteerLands for a steer), because a line the server gets lands within
//   ~100 ms and "Sending…"/"Steer pending…" would otherwise be states no
//   read could see. A held send is a slow network, nothing more.
// - Some spec steps are the page's own and happen in the same render as
//   the step before (the flush effect; a steer released by Stop lands in
//   the stopped turn). A read may therefore match a later node on the
//   path when every step in between is one of those automatic steps; the
//   walk then continues from there.
// - Where the spec forks and the product picks (a Stop puts the prompt
//   back or not; a stopped line lands or is swallowed), the product's
//   pick may not be the path's. When the page shows a state the spec
//   allows after that step, but not the path's, the walk ends there as
//   "not taken" (the Go adapter's fmbt.ErrNotImplemented); any other
//   state fails.
//
// made and stops are what the walk itself did; stop_send and stop_steer
// are the page's stoppedIds ref, which nothing on screen shows. They are
// not compared.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, type Turn } from '../../helpers/control';
import { loadPaths, roleState, type Step, walkTest } from '../../helpers/model';
import { loadGraph, walks } from '../../model/graph';
import { test, expect, type Serve } from '../../helpers/serve';

const SPEC = 'steer_queue';
const ROLE = 'Session#0';
type State = Record<string, unknown>;

// One serve per worker: the boot is paid once, and each path works only
// in the session it creates.
test.use({ workerServeOpts: { config: CONTROL_CONFIG } });

// ---- the graph ----

const OBSERVED = ['status', 'draft', 'queue', 'sending', 'steer', 'made', 'stops'];
// The page's and the child's own steps: nothing the person does.
const AUTOMATIC = new Set(['Flush', 'Start', 'SteerLands', 'SteerAsTurn', 'Swallow']);

const key = (s: State) => JSON.stringify(Object.keys(s).sort().map((k) => [k, s[k]]));
const seen = (s: State) => key(Object.fromEntries(OBSERVED.map((k) => [k, s[k]])));
const bare = (action: string) => action.startsWith(ROLE + '.') ? action.slice(ROLE.length + 1) : action;

const paths: Step[][] = loadPaths(SPEC);
// What the spec allows is read off walks that take every transition,
// whatever MODEL_COVER picks for the tests: the default walks reach every
// state but not every link, and a branch the page took then read as a
// state the spec forbids (Stop landing the steer and flushing the queue).
const allLinks = walks(loadGraph(path.resolve(__dirname, '..', '..', '..', 'model', 'testdata', SPEC)), 'transitions').paths.map((p) => p.trace);
const graph = new Map<string, { action: string; to: State }[]>();
for (const trace of allLinks) {
  for (let i = 1; i < trace.length; i++) {
    const from = roleState(ROLE, trace[i - 1].state), to = roleState(ROLE, trace[i].state), action = bare(trace[i].action);
    const out = graph.get(key(from)) ?? [];
    if (!out.some((e) => e.action === action && key(e.to) === key(to))) out.push({ action, to });
    graph.set(key(from), out);
  }
}

// Everything the spec allows once `action` is done in `from`: its
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

interface Held { route: Route; text: string }

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;   // llm-control's queue
  id: string;
  tag: string;   // unique per walk, in every line it writes
  made: number;
  stops: number;
  held: Held[];  // POST /prompt requests the network is holding, oldest first
}

// Model turn names are taken in lexical order and this worker's serve
// outlives the walk, so the counter is per worker (per process).
let turns = 0;

// Three block turns always queued: every model request the session makes
// (a new turn, a steered one, an answered question) is held until the
// walk answers it.
function topUp(c: Ctx): void {
  const queued = fs.readdirSync(c.dir).filter((f) => f.endsWith('.json')).length;
  for (let n = queued; n < 3; n++) queue(c.dir, `q${String(++turns).padStart(8, '0')}`, { mode: 'block', text: 'answered' });
}

function heldTurns(c: Ctx): string[] {
  return fs.readdirSync(c.dir).filter((f) => f.endsWith('.taken')).map((f) => f.slice(0, -'.taken'.length))
    .filter((n) => !fs.existsSync(path.join(c.dir, n + '.release')));
}

// Answers every model request in flight, as `turn` says when given.
function releaseAll(c: Ctx, turn?: Turn): void {
  for (const n of heldTurns(c)) turn ? releaseWith(c.dir, n, turn) : release(c.dir, n);
  topUp(c);
}

const composer = (c: Ctx) => c.page.locator('#composer');
const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const text = (c: Ctx, id: number) => `${c.tag} message ${id}`;

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 10_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`${SPEC}: waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

// Lets the oldest held send of that kind reach the server.
async function letGo(c: Ctx, steer: boolean): Promise<void> {
  const i = c.held.findIndex((h) => isSteer(c, h) === steer);
  if (i < 0) return;
  const [h] = c.held.splice(i, 1);
  await h.route.continue();
}

// Which held sends are steers is the page's call (live at send time); the
// walk remembers what the spec said when it pressed the key.
const steerTexts = new WeakMap<Ctx, Set<string>>();
const isSteer = (c: Ctx, h: Held) => steerTexts.get(c)?.has(h.text) ?? false;

// A step the page cannot take in this harness; the walk ends there.
class NotRealizable extends Error {}

type Act = (c: Ctx, before: State, after: State) => Promise<void>;

const actions: Record<string, Act> = {
  // Enter. The page ignores it while a send is on its way (send() returns
  // on `busy`), which a held send always is here.
  async Send(c, before, after) {
    if (c.held.length) throw new NotRealizable('Enter while a send is on its way: the page ignores the key (busy)');
    const t = text(c, c.made++);
    if ((after.sending as number[]).length === (before.sending as number[]).length) steerTexts.get(c)!.add(t);
    const n = c.held.length;
    await composer(c).fill(t);
    await composer(c).press('Enter');
    await until('the send to reach the network', () => c.held.length > n);
  },
  async Enqueue(c) {
    await composer(c).fill(text(c, c.made++));
    await composer(c).press('Control+Enter');
  },
  async WriteAnswer(c) { await composer(c).fill(`${c.tag} my answer`); },
  async Answer(c) { await composer(c).press('Enter'); },
  async Clear(c) { await composer(c).fill(''); },
  async RemoveQueued(c, before, after) {
    const b = before.queue as number[], a = after.queue as number[];
    const i = b.findIndex((m, k) => a[k] !== m);
    await c.page.locator('ol.queued li.queued-row').nth(i).getByRole('button', { name: 'Remove', exact: true }).click();
  },
  // Esc. The page waits for sends on their way before it interrupts; a
  // held steer is let go so the Stop can happen at all, and lands in the
  // stopped turn (the engine records queued steers before it cancels). A
  // held message stays held: Start or Swallow settles it.
  async Stop(c) {
    c.stops++;
    const running = (await read(c)).status === 'running';
    await composer(c).press('Escape');
    await letGo(c, true);
    // Whether it stopped is the read's to judge; this only waits for it.
    if (running) await until('the stop to reach the turn', async () => (await read(c)).status !== 'running').catch(() => {});
  },
  async Flush() { /* the page's flush effect */ },
  async Start(c) { await letGo(c, false); },
  async SteerLands(c) { await letGo(c, true); },
  async SteerAsTurn(c) { await letGo(c, true); },
  // The model answers. A steer on its way first reaches the turn, which
  // it then carries on; otherwise every request is answered until the
  // turn ends.
  async Finish(c) {
    if (c.held.some((h) => isSteer(c, h))) {
      await letGo(c, true);
      await until('the steer to land', async () => (await read(c)).steer !== 'pending');
      releaseAll(c);
      return;
    }
    await until('the turn to end', async () => {
      if ((await read(c)).status !== 'running') return true;
      releaseAll(c);
      return false;
    });
  },
  async Ask(c) {
    releaseAll(c, { mode: 'ok', call: { name: 'ask', args: { question: `${c.tag} which one?` } } } as Turn);
  },
  // Another client answers the question: the same POST, not this page.
  async AskGone(c) {
    const s = await (await c.serve.api.get(`/api/sessions/${c.id}`)).json();
    const ask = s.session.ask?.id;
    if (!ask) throw new Error(`AskGone: no question armed (status ${s.session.status})`);
    const res = await c.serve.api.post(`/api/sessions/${c.id}/answer`, { data: { text: `${c.tag} answered elsewhere`, ask } });
    if (!res.ok()) throw new Error(`answer: ${res.status()} ${await res.text()}`);
  },
  // The page's 4 s swallowedByStop timer. A stopped message is still held
  // on the network here, and the page's Stop waits for it before it
  // interrupts, so the line always lands first (Start): the swallow is a
  // race this harness cannot lose. Its state reads like the Flush after
  // it, so without this the walk took it as done and failed at Start.
  async Swallow(c, before) {
    if ((before.sending as number[]).length && c.held.some((h) => !isSteer(c, h))) {
      throw new NotRealizable('Swallow of a held message: the Stop waits for the send, so it lands (Start)');
    }
  },
  async end() { /* a quiet thread with its bounds used up */ },
};

// ---- readUiState: the role's fields, from the DOM only ----

const STATUS: Record<string, string> = { Running: 'running', 'Waiting for you': 'needs-you', Done: 'idle', Stopped: 'idle' };

async function read(c: Ctx): Promise<State> {
  const dom = await c.page.evaluate((id) => {
    const label = document.querySelector(`button.row[data-id="${id}"]`)?.getAttribute('aria-label') ?? '';
    const box = document.querySelector<HTMLTextAreaElement>('#composer');
    const primary = document.querySelector('.composer .btn-primary')?.getAttribute('aria-label') ?? '';
    const texts = (sel: string) => [...document.querySelectorAll(sel)].map((e) => e.textContent ?? '');
    const pending = [...document.querySelectorAll('.turn-sending-state')].map((s) => ({
      state: s.textContent ?? '',
      text: s.closest('section')?.querySelector('.prompt-bubble p')?.textContent ?? '',
    }));
    // An answer whose question is gone says so above the composer.
    const stale = texts('.composer-note-err').some((t) => t.includes('the one this answer was for is gone'));
    return { label, draft: box?.value ?? null, primary, stale, queued: texts('ol.queued .queued-text'), pending, failures: texts('.send-failed') };
  }, c.id);
  const num = (t: string) => Number(/message (\d+)$/.exec(t.trim())?.[1] ?? NaN);
  const word = dom.label.split(', ')[1] ?? '';
  return {
    status: STATUS[word] ?? `unknown: ${dom.label}`,
    draft: dom.draft === null ? 'no composer' : !dom.draft.trim() ? '' : dom.primary === 'Answer' ? (dom.stale ? 'stale' : 'answer') : 'msg',
    queue: dom.queued.map(num),
    sending: dom.pending.filter((p) => p.state.startsWith('Sending')).map((p) => num(p.text)),
    steer: dom.pending.some((p) => p.state.startsWith('Steer pending')) ? 'pending'
      : dom.failures.some((f) => f.includes('Steer dropped by Stop')) ? 'dropped' : 'none',
    made: c.made,
    stops: c.stops,
  };
}

const observed = (s: State) => Object.fromEntries(OBSERVED.map((k) => [k, s[k]]));

// Every node: no sideways scroll, the state's carriers on screen, and
// nothing logged as an error.
async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  const overflow = await c.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(row(c), `${where}: status not visible`).toBeVisible();
  await expect(composer(c), `${where}: composer not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

// Long enough for the page's 4 s swallow timer and a turn's catch-up.
const SETTLE_MS = 12_000;
const STABLE_MS = 400;

// Waits for the page to show node `n`, or a later one reached by
// automatic steps only. Returns the node it matched, or -1 when the page
// settled on a state the spec allows after the step but the path did not
// take.
async function settle(c: Ctx, trace: Step[], n: number, where: string, errors: string[]): Promise<number> {
  const want: string[] = [];
  for (let m = n; m < trace.length; m++) {
    if (m > n && !AUTOMATIC.has(bare(trace[m].action))) break;
    want.push(seen(roleState(ROLE, trace[m].state)));
  }
  const allowed = n > 0 ? allowedAfter(roleState(ROLE, trace[n - 1].state), bare(trace[n].action)) : new Set<string>();
  const deadline = Date.now() + SETTLE_MS;
  let last: State = {};
  // A match must hold for STABLE_MS: mid-Stop, the old turn still reads
  // running with the steer landed, which is the SteerAsTurn node for a
  // moment and was taken for it.
  let held = '', since = 0;
  for (;;) {
    last = await read(c);
    const i = want.indexOf(seen(last));
    if (seen(last) !== held) { held = seen(last); since = Date.now(); }
    if (i >= 0 && Date.now() - since >= STABLE_MS) return n + i;
    if (Date.now() > deadline) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  if (allowed.has(seen(last))) return -1;
  expect(observed(last), `${where}: state${errors.length ? ` (console: ${errors.join(' | ')})` : ''}`).toEqual(observed(roleState(ROLE, trace[n].state)));
  return n;
}

function saveTranscripts(c: Ctx, title: string): void {
  const root = process.env.MODEL_TRACE_DIR;
  if (!root) return;
  const dir = path.join(root, SPEC);
  fs.mkdirSync(dir, { recursive: true });
  const src = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  const slug = title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '');
  if (fs.existsSync(src)) fs.copyFileSync(src, path.join(dir, `${slug}-${c.id}.jsonl`));
}

test.describe(`model: ${SPEC}`, () => {
  paths.forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => bare(s.action)).join(' → ');
    walkTest(SPEC)(`path ${i}: ${walk}`, async ({ sharedServe: serve, page }, info) => {
      test.setTimeout(150_000);
      const errors: string[] = [];
      page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e.stack ?? e)));

      const c: Ctx = { page, serve, dir: controlDir(serve.home), id: '', tag: `p${i}w${info.workerIndex}r${info.retry}`, made: 0, stops: 0, held: [] };
      steerTexts.set(c, new Set());
      fs.mkdirSync(c.dir, { recursive: true });
      topUp(c);
      // Init: the thread is open and its first turn is running (held).
      c.id = await serve.newSession(`${c.tag} first prompt`);
      await page.route(`**/api/sessions/${c.id}/prompt`, (route) => {
        c.held.push({ route, text: route.request().postDataJSON()?.text ?? '' });
      });
      await page.goto(`${serve.url}/#/s/${c.id}`);

      let reached = 0;
      try {
        for (let n = 0; n < trace.length; n++) {
          const name = n === 0 ? 'Init' : bare(trace[n].action);
          const where = `step ${n} (${name})`;
          if (n > 0 && n > reached) {
            const act = actions[name];
            if (!act) throw new Error(`${SPEC}: no action for ${trace[n].action}`);
            try {
              await act(c, roleState(ROLE, trace[n - 1].state), roleState(ROLE, trace[n].state));
            } catch (e) {
              if (!(e instanceof NotRealizable)) throw e;
              info.annotations.push({ type: 'not-realizable', description: `${where}: ${e.message}` });
              break;
            }
          }
          if (n <= reached && n > 0) continue; // an automatic step the page already took
          const m = await settle(c, trace, n, where, errors);
          if (m < 0) {
            info.annotations.push({ type: 'not-taken', description: `${where}: the page took another branch the spec allows` });
            break;
          }
          await invariants(c, errors, where);
          reached = m;
        }
      } finally {
        info.annotations.push({ type: 'reached', description: String(reached) });
        // The page leaves first: open, it would flush its queue into the
        // turn the archive below cancels, and the transcript is copied
        // before that cancel, as the walk left it.
        await page.goto('about:blank');
        for (const h of c.held.splice(0)) await h.route.abort().catch(() => {});
        saveTranscripts(c, info.title);
        // Archiving kills the child, so it cannot take the next walk's turns.
        await serve.api.post(`/api/sessions/${c.id}/archive`).catch(() => {});
        releaseAll(c);
      }
    });
  });
});
