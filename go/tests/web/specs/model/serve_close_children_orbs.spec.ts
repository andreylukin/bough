// The browser walk of go/tests/model/specs/serve_close_children_orbs.fizz:
// serve going down (Close, a crash) and coming back while one parent's
// background agents run and queue and one of them owns a project orb.
// Every covering path is walked against serve as a person has it open:
// one tab on the parent P (its Work button and its agents' reports),
// one on the agent A (its orb chip and the composer a person resumes it
// with). At every node the page must show what the spec derives from
// the state, and must catch up after each restart without a reload.
//
// Not helpers/serve.ts: a `bough serve` process opens the host's
// container runtime, which a test may never touch, and "closing" (inside
// sup.Close) cannot be reached from outside. The backend is
// TestServeCloseChildrenOrbsBrowserServe (go/tests/model/mbt): the Go
// walk's adapter, its supervisor on container.Fake, stub agent processes,
// served at one address that stops listening while serve is down. It
// checks the full room against the spec at every step (the fields with no
// pixels: a_msg, dups, the container and its image, closing vs down);
// the page is checked on what it shows.
import { spawn, execFileSync, type ChildProcess } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect, type Page } from '@playwright/test';
import { loadPaths, roleState } from '../../helpers/model';

const SPEC = 'serve_close_children_orbs';
const ROLE = 'Room#0';
type State = Record<string, unknown>;

interface Backend { ui: string; model: string; home: string }

const goDir = path.resolve(__dirname, '..', '..', '..', '..');

// The backend binary: $MODEL_SCCO_BIN, else built once per worker.
function backendBin(worker: number): string {
  if (process.env.MODEL_SCCO_BIN) return process.env.MODEL_SCCO_BIN;
  const out = path.join(os.tmpdir(), `bough-scco-model-${process.pid}-${worker}.test`);
  execFileSync('go', ['test', '-c', '-o', out, './tests/model/mbt/'], { cwd: goDir, stdio: 'inherit' });
  return out;
}

const test = base.extend<{}, { backend: Backend }>({
  backend: [async ({}, use, info) => {
    const bin = backendBin(info.workerIndex);
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-scco-home-'));
    const urls = path.join(home, 'urls.json');
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home, MODEL_SCCO_SERVE: urls };
    for (const k of Object.keys(env)) if (k.endsWith('_API_KEY')) delete env[k];
    const child: ChildProcess = spawn(bin, ['-test.run', '^TestServeCloseChildrenOrbsBrowserServe$', '-test.v'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    const log: string[] = [];
    child.stdout?.on('data', (d) => log.push(String(d)));
    child.stderr?.on('data', (d) => log.push(String(d)));
    const exited = new Promise<void>((r) => child.once('exit', () => r()));
    const deadline = Date.now() + 30_000;
    while (!fs.existsSync(urls)) {
      if (child.exitCode !== null || Date.now() > deadline) throw new Error(`backend did not start:\n${log.join('')}`);
      await new Promise((r) => setTimeout(r, 50));
    }
    try {
      await use(JSON.parse(fs.readFileSync(urls, 'utf8')));
    } finally {
      child.stdin?.end();
      const t = setTimeout(() => child.kill('SIGKILL'), 5000);
      await exited;
      clearTimeout(t);
      fs.rmSync(home, { recursive: true, force: true });
      if (!process.env.MODEL_SCCO_BIN) fs.rmSync(bin, { force: true });
    }
  }, { scope: 'worker', timeout: 180_000 }],
});

interface Ctx {
  page: Page;   // A's thread
  work: Page;   // P's thread
  b: Backend;
  ids: { p: string; a: string; b: string; c: string };
  home: string;
  resumes: number;
}

async function call(b: Backend, route: string): Promise<{ state?: State; error?: string; p?: string; a?: string; b?: string; c?: string; home?: string }> {
  const res = await fetch(b.model + route, { method: 'POST' });
  return res.json();
}

async function must(b: Backend, route: string): Promise<State> {
  const r = await call(b, route);
  if (r.error) throw new Error(`${route}: ${r.error}`);
  return r.state ?? {};
}

// --- the spec's views of a state (specs/serve_close_children_orbs.fizz) ---

const alive = (s: State) => s.a === 'r' || s.a === 'i';

function orbView(s: State): string {
  const f = s.file as string;
  return ['building', 'starting', 'running'].includes(f) && !alive(s) ? 'stopped' : f;
}

// Reports P holds of an agent's turns: none while queued, all but the
// open one while it runs (or its cut one is owed), every one once it
// was told. A has had 1 + resumes turns, B and C one each.
function reports(v: unknown, turns: number): number {
  if (v === 'q') return 0;
  return v === 'r' || v === 'xo' ? turns - 1 : turns;
}

// What the page shows of a state. While serve does not answer (closing
// or down) the page can only say it is offline: every other field is
// the last thing it saw, and the spec makes no claim about that.
function view(s: State, c: Ctx): State {
  if (s.serve !== 'up') return { serve: 'down', orb: '-', running: '-', queued: '-', reports: '-' };
  const n = (v: string) => ['a', 'b', 'c'].filter((k) => s[k] === v).length;
  return {
    serve: 'up',
    orb: orbView(s),
    running: n('r'),
    queued: n('q'),
    reports: { a: reports(s.a, 1 + c.resumes), b: reports(s.b, 1), c: reports(s.c, 1) },
  };
}

// --- readUiState: the same, off the DOM ---

// The chip's words (web/src/mode.tsx): "proj · <word>", or the step in
// progress and its time while the orb is busy.
const CHIP: [RegExp, string][] = [
  [/^(Building image|Building)\b/, 'building'],
  [/^(Syncing repos|Making worktrees|Starting container|Running resume\.sh|Starting)\b/, 'starting'],
  [/^Running$/, 'running'],
  [/^Stopped$/, 'stopped'],
];

const chip = (c: Ctx) => c.page.locator('header.thread-head .mode-chip').first();

async function offline(p: Page): Promise<boolean> {
  return (await p.locator('.side-fresh', { hasText: /Updates delayed|Sessions unavailable/ }).count()) > 0
    || (await p.locator('.rt-paused').count()) > 0;
}

async function readUiState(c: Ctx): Promise<State> {
  if (await offline(c.page) || await offline(c.work)) return { serve: 'down', orb: '-', running: '-', queued: '-', reports: '-' };
  const text = ((await chip(c).textContent({ timeout: 1000 }).catch(() => '')) ?? '').trim();
  const word = text.split(' · ').slice(1).join(' · ');
  const orb = CHIP.find(([re]) => re.test(word))?.[1] ?? `unknown: ${text}`;
  const work = (await c.work.locator('button.work-summary').getAttribute('aria-label', { timeout: 1000 }).catch(() => '')) ?? '';
  const count = (w: string) => Number(new RegExp(`\\b(\\d+) ${w}\\b`).exec(work)?.[1] ?? 0);
  // Each report is a notice in P's thread named by the agent's title,
  // which is its task ("task a"; mbt Init): the name's full text.
  const names = await c.work.locator('.agent-notice .agent-notice-name').evaluateAll((els) => els.map((e) => e.getAttribute('title') ?? ''));
  const told = (k: string) => names.filter((n) => n === `task ${k}`).length;
  return {
    serve: 'up',
    orb,
    running: count('running'),
    queued: count('queued'),
    reports: { a: told('a'), b: told('b'), c: told('c') },
  };
}

// --- actions ---

// A person messages A: the composer on A's thread. Everything else is
// serve's process (SIGTERM, a crash, launchd starting it), its agents'
// processes (a turn closing, the orb's state.json) or a reaper tick: no
// control on the page, so the backend plays it.
async function resumeA(c: Ctx): Promise<State> {
  await must(c.b, '/model/begin/ResumeA');
  const msg = `message ${++c.resumes}`;
  const sent = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.ids.a}/prompt`) && r.request().method() === 'POST');
  await c.page.locator('#composer').fill(msg);
  await c.page.getByRole('button', { name: 'Send', exact: true }).click();
  const res = await sent;
  expect(res.ok(), `ResumeA: send answered ${res.status()}`).toBe(true);
  return must(c.b, '/model/after/ResumeA');
}

async function act(c: Ctx, name: string): Promise<State> {
  if (name === 'ResumeA') return resumeA(c);
  return must(c.b, `/model/act/${name}`);
}

// Every node: no sideways scroll on either tab, A's orb chip on screen,
// no console errors but the refused requests of a serve that is down.
async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  for (const p of [c.page, c.work]) {
    const overflow = await p.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  }
  await expect(chip(c), `${where}: A's orb chip not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

// Chromium logs each request a down serve refuses (or drops mid-stream).
const REFUSED = /^Failed to load resource: net::ERR_(CONNECTION_REFUSED|EMPTY_RESPONSE|CONNECTION_RESET|INCOMPLETE_CHUNKED_ENCODING)$/;

function saveTranscript(c: Ctx, title: string): void {
  const root = process.env.MODEL_TRACE_DIR;
  if (!root || !c.home) return;
  const dir = path.join(root, SPEC);
  fs.mkdirSync(dir, { recursive: true });
  const src = path.join(c.home, '.bough', 'history', c.ids.a + '.jsonl');
  const slug = title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '');
  if (fs.existsSync(src)) fs.copyFileSync(src, path.join(dir, `${slug}-${c.ids.a}.jsonl`));
}

test.describe(`model: ${SPEC}`, () => {
  loadPaths(SPEC).forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test(`path ${i}: ${walk}`, async ({ page, backend }, info) => {
      test.setTimeout(30_000 + 8_000 * trace.length);
      const errors: string[] = [];
      const work = await page.context().newPage();
      for (const p of [page, work]) {
        p.on('console', (m) => { if (m.type() === 'error' && !REFUSED.test(m.text())) errors.push(m.text()); });
        p.on('pageerror', (e) => errors.push(String(e)));
        await p.clock.install();
      }
      const init = await call(backend, '/model/init');
      if (init.error) throw new Error(`Init: ${init.error}`);
      const c: Ctx = {
        page, work, b: backend,
        ids: { p: init.p!, a: init.a!, b: init.b!, c: init.c! },
        home: init.home ?? '', resumes: 0,
      };
      try {
        await work.goto(`${backend.ui}/#/s/${c.ids.p}`);
        await page.goto(`${backend.ui}/#/s/${c.ids.a}`);
        await page.bringToFront();
        for (const [n, step] of trace.entries()) {
          const name = n === 0 ? 'Init' : step.action.slice(ROLE.length + 1);
          const where = `step ${n} (${name})`;
          const want = roleState(ROLE, step.state);
          const got = n === 0 ? init.state : await act(c, name);
          expect(got, `${where}: backend state`).toEqual(want);
          await expect.poll(async () => {
            await page.clock.fastForward(5_000);
            await work.clock.fastForward(5_000);
            return readUiState(c);
          }, { message: `${where}: page`, timeout: 15_000 }).toEqual(view(want, c));
          await invariants(c, errors, where);
        }
      } catch (e) {
        // The failure context shows A's tab; P's is the other half.
        fs.writeFileSync(info.outputPath('p-tab.md'), await work.locator('body').ariaSnapshot().catch(() => ''));
        throw e;
      } finally {
        saveTranscript(c, info.title);
        await call(backend, '/model/cleanup');
        await work.close();
      }
    });
  });
});
