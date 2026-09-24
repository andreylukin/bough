// The browser walk of go/tests/model/specs/orb_lifecycle.fizz: a project
// session's orb as the control room shows it. Every covering path in
// testdata/orb_lifecycle/paths.json is walked against a serve on a fake
// container runtime, and at every node the thread's header must show
// what the spec derives from the state (status, phase, Stop orb, the
// portal, the build log), plus the invariants every screen owes.
//
// Not helpers/serve.ts: a `bough serve` process always opens the host's
// container runtime, and a test may never touch a real one. The backend
// is TestOrbLifecycleBrowserServe (go/tests/model/mbt): the same API and
// supervisor on container.Fake, served in process, with the MBT
// adapter playing the session child's state.json writes. One per worker,
// like helpers/serve.ts's workerServe; each path is its own session.
//
// Most of the spec's fields have no pixels (who owns the orb, serve's
// stop mark, the snapshot, idleness): those are checked on the backend
// at every node — the adapter's observation must equal the spec's state,
// and serve's API its views — and the page is checked on what it shows.
import { spawn, execFileSync, type ChildProcess } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect, type Page } from '@playwright/test';
import { loadPaths, roleState } from '../../helpers/model';

const ROLE = 'Orb#0';
type State = Record<string, unknown>;

interface Backend { ui: string; model: string; home: string }

const goDir = path.resolve(__dirname, '..', '..', '..', '..');

// The backend binary: $MODEL_ORB_BIN, else built once per worker (go's
// build cache makes every build after the first a link).
function backendBin(worker: number): string {
  if (process.env.MODEL_ORB_BIN) return process.env.MODEL_ORB_BIN;
  const out = path.join(os.tmpdir(), `bough-orb-model-${process.pid}-${worker}.test`);
  execFileSync('go', ['test', '-c', '-o', out, './tests/model/mbt/'], { cwd: goDir, stdio: 'inherit' });
  return out;
}

const test = base.extend<{}, { backend: Backend }>({
  backend: [async ({}, use, info) => {
    const bin = backendBin(info.workerIndex);
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-orb-home-'));
    const urls = path.join(home, 'urls.json');
    // HOME is the Go side's own temp dir too (it sets it), never the real one.
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home, MODEL_ORB_SERVE: urls };
    for (const k of Object.keys(env)) if (k.endsWith('_API_KEY')) delete env[k];
    const child: ChildProcess = spawn(bin, ['-test.run', '^TestOrbLifecycleBrowserServe$', '-test.v'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    const log: string[] = [];
    child.stdout?.on('data', (d) => log.push(String(d)));
    child.stderr?.on('data', (d) => log.push(String(d)));
    const exited = new Promise<void>((r) => child.once('exit', () => r()));
    const deadline = Date.now() + 30_000;
    while (!fs.existsSync(urls)) {
      if (child.exitCode !== null || Date.now() > deadline) throw new Error(`orb backend did not start:\n${log.join('')}`);
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
      if (!process.env.MODEL_ORB_BIN) fs.rmSync(bin, { force: true });
    }
  }, { scope: 'worker', timeout: 180_000 }],
});

interface Ctx {
  page: Page; b: Backend; id: string; slug: string;
  // The console errors a step is expected to log (see Remove).
  expected: string[];
}

async function call(b: Backend, route: string): Promise<{ state: State; error?: string; id?: string; slug?: string }> {
  const res = await fetch(b.model + route, { method: 'POST' });
  const body = await res.json();
  return body;
}

// --- the spec's derived views (specs/orb_lifecycle.fizz), line for line ---

function status(o: State): string {
  const f = o.file as string;
  if (f === 'running' || f === 'starting') return o.owner === 'none' || o.mark ? 'stopped' : f;
  return f;
}

function up(o: State): boolean {
  const f = o.file;
  if (f === 'failed') return !!o.snap;
  if (f !== 'running' && f !== 'starting') return false;
  const u = f === 'running' ? true : !!o.snap;
  if (o.owner === 'none') return u && !!o.snap;
  if (o.mark) return false;
  return u;
}

// What the thread header shows of a state: the orb's status, the phase
// while it is busy (the chip names the step in progress), whether Stop
// orb is offered, whether the portal says a port is live, and whether
// the build log is offered as live.
function view(o: State): State {
  const s = status(o);
  return {
    status: s,
    phase: s === 'starting' || s === 'building' ? o.phase : '',
    up: up(o),
    portal: !!o.portal && o.owner !== 'none',
    buildLog: o.file === 'building' ? 'building' : 'done',
  };
}

// --- readUiState: the same, off the DOM ---

const WORD: Record<string, string> = { Pending: 'none', Building: 'building', Starting: 'starting', Running: 'running', Stopped: 'stopped' };
// The chip's busy words (web/src/mode.tsx BUSY_WORD) as the spec's phases.
const STEP: Record<string, { status: string; phase: string }> = {
  'Syncing repos': { status: 'starting', phase: 'prepare' },
  'Making worktrees': { status: 'starting', phase: 'prepare' },
  'Building image': { status: 'building', phase: 'build' },
  'Starting container': { status: 'starting', phase: 'container' },
  'Running resume.sh': { status: 'starting', phase: 'resume' },
};

const head = (c: Ctx) => c.page.locator('header.thread-head');
// The chip carries the orb's state: "<project> · <word>", or the step
// and its time while busy; a failed setup is its own mark.
const chip = (c: Ctx) => head(c).locator('.mode-chip, .setup-failed').first();

async function readUiState(c: Ctx): Promise<State> {
  const h = head(c);
  let s = 'unknown', phase = '';
  if (await h.locator('.setup-failed').count()) s = 'failed';
  else {
    const text = ((await h.locator('.mode-chip').first().textContent({ timeout: 1000 }).catch(() => '')) ?? '').trim();
    const word = text.split(' · ').slice(1).join(' · ');
    const step = Object.entries(STEP).find(([w]) => word.startsWith(w + ' '));
    if (step) ({ status: s, phase } = step[1]);
    else s = WORD[word] ?? `unknown: ${text}`;
  }
  const portal = (await h.getByRole('button', { name: /^Portal — /, exact: false }).getAttribute('aria-label', { timeout: 1000 }).catch(() => '')) ?? '';
  return {
    status: s,
    phase,
    up: await h.getByRole('button', { name: 'Stop orb', exact: true }).isVisible(),
    portal: / is live$| are live$/.test(portal),
    buildLog: (await h.getByRole('button', { name: 'Show the live build log' }).count()) > 0 ? 'building' : 'done',
  };
}

// --- actions ---

// The page's own: Stop orb in the thread header, and Remove… on the
// project's orb page (the only place it is offered) behind its confirm.
async function pageAction(c: Ctx, name: string, drive: (before: State) => Promise<void>): Promise<State> {
  const begun = await call(c.b, `/model/begin/${name}`);
  if (begun.error) throw new Error(`${name}: ${begun.error}`);
  await drive(begun.state);
  const r = await call(c.b, '/model/record');
  if (r.error) throw new Error(`${name}: ${r.error}`);
  return r.state;
}

const UI: Record<string, (c: Ctx, before: State) => Promise<void>> = {
  async StopOrb(c) {
    const done = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/orb/stop`) && r.request().method() === 'POST');
    await head(c).getByRole('button', { name: 'Stop orb', exact: true }).click();
    await done;
  },
  // With uncommitted work the confirm says so and still offers Remove orb;
  // serve refuses it (409, after killing its own child: see the spec's
  // notes), the page shows why, and the browser logs the 409 — the one
  // console error a walk expects.
  async Remove(c, before) {
    await c.page.goto(`${c.b.ui}/#/projects/${c.slug}/orb`);
    const row = c.page.locator('.orb-row').first();
    const done = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/orb`) && r.request().method() === 'DELETE');
    await row.getByRole('button', { name: 'Remove…' }).click();
    const dialog = c.page.getByRole('dialog');
    if (before.dirty) await expect(dialog).toContainText('Uncommitted changes');
    await dialog.getByRole('button', { name: 'Remove orb' }).click();
    const res = await done;
    if (before.dirty) {
      expect(res.status(), 'Remove with uncommitted work').toBe(409);
      c.expected.push('Failed to load resource: the server responded with a status of 409 (Conflict)');
      await expect(c.page.getByText(/uncommitted changes in .*; commit or discard them first/)).toBeVisible();
    }
    await c.page.goto(`${c.b.ui}/#/s/${c.id}`);
  },
};

// Everything else is the session child's writes (state.json, build.json,
// its own process), time passing (idle, the snapshot's TTL, a reaper
// tick) or serve's supervisor killing its child (archive and EndProject
// come down to it): none has a control on the page, so the backend plays
// it, as the MBT adapter does.
async function act(c: Ctx, name: string): Promise<State> {
  if (UI[name]) return pageAction(c, name, (before) => UI[name](c, before));
  const r = await call(c.b, `/model/act/${name}`);
  if (r.error) throw new Error(`${name}: ${r.error}`);
  return r.state;
}

// Every node: no sideways scroll, the chip on screen, no console errors.
async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  for (const e of c.expected.splice(0)) {
    const i = errors.indexOf(e);
    if (i >= 0) errors.splice(i, 1);
  }
  const overflow = await c.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(chip(c), `${where}: orb status not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

test.describe('model: orb_lifecycle', () => {
  loadPaths('orb_lifecycle').forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test(`path ${i}: ${walk}`, async ({ page, backend }) => {
      test.setTimeout(60_000);
      const errors: string[] = [];
      page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e)));
      await page.clock.install();

      const init = await call(backend, '/model/init');
      if (init.error) throw new Error(`Init: ${init.error}`);
      const c: Ctx = { page, b: backend, id: init.id!, slug: init.slug!, expected: [] };
      try {
        await page.goto(`${backend.ui}/#/s/${c.id}`);
        for (const [n, step] of trace.entries()) {
          const name = n === 0 ? 'Init' : step.action.slice(ROLE.length + 1);
          const where = `step ${n} (${name})`;
          const want = roleState(ROLE, step.state);
          const got = n === 0 ? init.state : await act(c, name);
          expect(got, `${where}: backend state`).toEqual(want);
          await expect.poll(async () => {
            await page.clock.fastForward(5_000);
            return readUiState(c);
          }, { message: `${where}: page`, timeout: 10_000 }).toEqual(view(want));
          await invariants(c, errors, where);
        }
      } finally {
        await call(backend, '/model/cleanup');
      }
    });
  });
});
