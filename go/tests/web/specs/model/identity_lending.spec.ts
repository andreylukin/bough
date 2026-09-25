// The browser walk of go/tests/model/specs/identity_lending.fizz: the
// $HOME dirs a project lends its container, as the project's orb page
// shows them. Every walk over testdata/identity_lending is driven against
// a serve on a fake container runtime, and at every node the page must
// show what the spec says: the definition's entry in the project.yml
// editor, the session's container in the Sessions table, and the one
// action its row offers; plus the invariants every screen owes.
//
// Not helpers/serve.ts, for orb_lifecycle's reason: a `bough serve`
// process opens the host's container runtime. The backend is
// TestIdentityLendingBrowserServe (go/tests/model/mbt): the same API on
// container.Fake in process, with the MBT adapter playing the session's
// process, the host's $HOME and the guest. One per worker; each walk is
// its own project.
//
// What the page takes: the definition edits (the editor's Save), Stop
// orb and Remove orb. Most of the spec's fields have no pixels (the host
// dir, the container's mounts, drift, stale, the other project): those
// are checked on the backend at every node, where the adapter's
// observation must equal the spec's state.
import { spawn, execFileSync, type ChildProcess } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect, type Page } from '@playwright/test';
import { loadPaths, roleState } from '../../helpers/model';

const ROLE = 'Lend#0';
type State = Record<string, unknown>;

interface Backend { ui: string; model: string; home: string }

const goDir = path.resolve(__dirname, '..', '..', '..', '..');

// The backend binary: $MODEL_LEND_BIN, else built once per worker.
function backendBin(worker: number): string {
  if (process.env.MODEL_LEND_BIN) return process.env.MODEL_LEND_BIN;
  const out = path.join(os.tmpdir(), `bough-lend-model-${process.pid}-${worker}.test`);
  execFileSync('go', ['test', '-c', '-o', out, './tests/model/mbt/'], { cwd: goDir, stdio: 'inherit' });
  return out;
}

const test = base.extend<{}, { backend: Backend }>({
  backend: [async ({}, use, info) => {
    const bin = backendBin(info.workerIndex);
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-lend-home-'));
    const urls = path.join(home, 'urls.json');
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home, MODEL_LEND_SERVE: urls };
    for (const k of Object.keys(env)) if (k.endsWith('_API_KEY')) delete env[k];
    const child: ChildProcess = spawn(bin, ['-test.run', '^TestIdentityLendingBrowserServe$', '-test.v'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    const log: string[] = [];
    child.stdout?.on('data', (d) => log.push(String(d)));
    child.stderr?.on('data', (d) => log.push(String(d)));
    const exited = new Promise<void>((r) => child.once('exit', () => r()));
    const deadline = Date.now() + 30_000;
    while (!fs.existsSync(urls)) {
      if (child.exitCode !== null || Date.now() > deadline) throw new Error(`lend backend did not start:\n${log.join('')}`);
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
      if (!process.env.MODEL_LEND_BIN) fs.rmSync(bin, { force: true });
    }
  }, { scope: 'worker', timeout: 180_000 }],
});

interface Ctx {
  page: Page; b: Backend; id: string; slug: string;
  // The console errors a step is expected to log (the refused save).
  expected: string[];
}

async function call(b: Backend, route: string): Promise<{ state: State; error?: string; id?: string; slug?: string }> {
  const res = await fetch(b.model + route, { method: 'POST' });
  return res.json();
}

// The entries the backend's adapter classifies (identity_lending_test.go):
// .aws is ok, keys (a $HOME symlink to ~/.ssh) is the hazard, .ssh is
// refused by the deny list.
const YAML: Record<string, string> = {
  none: 'repos: []\n',
  ok: 'repos: []\nidentity: [.aws]\n',
  hazard: 'repos: []\nidentity: [keys]\n',
  denied: 'repos: []\nidentity: [.ssh:rw]\n',
};

// What the page shows of a state: the definition's entry, the
// container's status, and the row's action (Stop orb while it runs,
// Remove… once it is stopped, nothing without one).
function view(l: State): State {
  return { d: l.d, ctr: l.ctr, stop: l.ctr === 'running', remove: l.ctr === 'stopped' };
}

// --- readUiState: the same, off the DOM ---

const panel = (c: Ctx) => c.page.locator('.proj-orb');
const editor = (c: Ctx) => panel(c).getByRole('textbox', { name: 'project.yml', exact: true });
const rows = (c: Ctx) => panel(c).locator('.orb-row');

function kind(text: string): string {
  const m = /^identity:\s*\[(.*)\]\s*$/m.exec(text);
  const ids = (m?.[1] ?? '').split(',').map((s) => s.trim().replace(/:(ro|rw)$/, ''));
  if (ids.includes('.aws')) return 'ok';
  if (ids.includes('keys')) return 'hazard';
  if (ids.includes('.ssh')) return 'denied';
  return 'none';
}

const CTR: Record<string, string> = { Running: 'running', Stopped: 'stopped' };

async function readUiState(c: Ctx): Promise<State> {
  const n = await rows(c).count();
  let ctr = 'missing', stop = false, remove = false;
  if (n > 1) ctr = `${n} rows`;
  else if (n === 1) {
    const r = rows(c).first();
    const word = ((await r.locator('.orb-st').textContent({ timeout: 1000 }).catch(() => '')) ?? '').trim();
    ctr = CTR[word] ?? `unknown: ${word}`;
    stop = await r.getByRole('button', { name: 'Stop orb', exact: true }).isVisible();
    remove = await r.getByRole('button', { name: 'Remove…', exact: true }).isVisible();
  }
  return { d: kind(await editor(c).inputValue({ timeout: 1000 }).catch(() => '')), ctr, stop, remove };
}

// --- actions ---

async function save(c: Ctx, text: string, refused = false): Promise<void> {
  await panel(c).getByRole('tab', { name: 'project.yml' }).click();
  const was = await editor(c).inputValue();
  await editor(c).fill(text);
  const put = c.page.waitForResponse((r) => r.request().method() === 'PUT' && r.url().includes('/orb/files/project.yml'));
  await panel(c).getByRole('button', { name: 'Save', exact: true }).click();
  const res = await put;
  if (!refused) {
    expect(res.ok(), 'save project.yml').toBe(true);
    await expect(panel(c).getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
    await expect(panel(c).locator('.orb-save-err')).toHaveCount(0);
    return;
  }
  // The deny list's refusal: a 400 the editor shows beside it, naming
  // the entry. The person then takes the entry back out.
  expect(res.status(), 'a denied entry').toBe(400);
  c.expected.push('Failed to load resource: the server responded with a status of 400 (Bad Request)');
  await expect(panel(c).locator('.orb-save-err')).toContainText('.ssh');
  await editor(c).fill(was);
  await expect(panel(c).getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
}

// The page's own: the editor's Save, and a session row's Stop orb or
// Remove… behind its confirm. The backend checks the require first and
// records the step after.
const UI: Record<string, (c: Ctx) => Promise<void>> = {
  AddDir: (c) => save(c, YAML.ok),
  AddHazard: (c) => save(c, YAML.hazard),
  AddDenied: (c) => save(c, YAML.denied, true),
  RemoveDir: (c) => save(c, YAML.none),
  async StopOrb(c) {
    const done = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/orb/stop`) && r.request().method() === 'POST');
    await rows(c).first().getByRole('button', { name: 'Stop orb', exact: true }).click();
    expect((await done).ok(), 'Stop orb').toBe(true);
  },
  async RemoveOrb(c) {
    const done = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/orb`) && r.request().method() === 'DELETE');
    await rows(c).first().getByRole('button', { name: 'Remove…', exact: true }).click();
    await c.page.getByRole('dialog').getByRole('button', { name: 'Remove orb' }).click();
    expect((await done).ok(), 'Remove orb').toBe(true);
  },
};

async function pageAction(c: Ctx, name: string): Promise<State> {
  const begun = await call(c.b, `/model/begin/${name}`);
  if (begun.error) throw new Error(`${name}: ${begun.error}`);
  await UI[name](c);
  const r = await call(c.b, '/model/record');
  if (r.error) throw new Error(`${name}: ${r.error}`);
  return r.state;
}

// Everything else is the session's process (start, exec, exit), the
// host's $HOME, or the guest (its own project.yml write, the relay):
// none has a control on the page, so the backend plays it.
async function act(c: Ctx, name: string): Promise<State> {
  if (UI[name]) return pageAction(c, name);
  const r = await call(c.b, `/model/act/${name}`);
  if (r.error) throw new Error(`${name}: ${r.error}`);
  return r.state;
}

// Every node: no sideways scroll, the editor and its Sessions on screen,
// no unsaved draft left behind, no console errors.
async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  for (const e of c.expected.splice(0)) {
    const i = errors.indexOf(e);
    if (i >= 0) errors.splice(i, 1);
  }
  const overflow = await c.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(editor(c), `${where}: project.yml not visible`).toBeVisible();
  await expect(panel(c).getByRole('heading', { name: 'Sessions' }), `${where}: Sessions not visible`).toBeVisible();
  await expect(panel(c).getByRole('tab', { name: 'project.yml' }), `${where}: a draft left unsaved`).toHaveText('project.yml');
  expect(errors, `${where}: console errors`).toEqual([]);
}

test.describe('model: identity_lending', () => {
  loadPaths('identity_lending').forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test(`path ${i}: ${walk.length > 200 ? walk.slice(0, 200) + '…' : walk}`, async ({ page, backend }) => {
      test.setTimeout(30_000 + trace.length * 5_000);
      const errors: string[] = [];
      page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e)));
      await page.clock.install();

      const init = await call(backend, '/model/init');
      if (init.error) throw new Error(`Init: ${init.error}`);
      const c: Ctx = { page, b: backend, id: init.id!, slug: init.slug!, expected: [] };
      try {
        await page.goto(`${backend.ui}/#/projects/${c.slug}/orb`);
        for (const [n, step] of trace.entries()) {
          const name = n === 0 ? 'Init' : step.action === 'end' ? 'end' : step.action.slice(ROLE.length + 1);
          const where = `step ${n} (${name})`;
          const want = roleState(ROLE, step.state);
          const got = n === 0 ? init.state : name === 'end' ? want : await act(c, name);
          expect(got, `${where}: backend state`).toEqual(want);
          await expect.poll(async () => {
            await page.clock.fastForward(5_000);
            return readUiState(c);
          }, { message: `${where}: page`, timeout: 10_000 }).toEqual(view(want));
          await invariants(c, errors, where);
          // The deny list's refusal is a self-link, which only the
          // transitions walks take: it is taken through the page once per
          // walk too, from the first node, which lists nothing.
          if (n === 0) {
            await act(c, 'AddDenied');
            expect(await readUiState(c), `${where}: after AddDenied`).toEqual(view(want));
            await invariants(c, errors, `${where} + AddDenied`);
          }
        }
      } finally {
        await call(backend, '/model/cleanup');
      }
    });
  });
});
