// The browser walk of go/tests/model/specs/orb_remove_data_safety.fizz:
// what a remove of a project session's orb may delete, as the project's
// orb page shows it. Every walk over testdata/orb_remove_data_safety is
// driven against a serve on a fake container runtime; at every node the
// backend's observation must equal the spec's state, and the page must
// show what the spec derives from it: whether the orb's row is listed,
// the Remove confirm the page opened (and which one), and why the page's
// own remove was refused or failed.
//
// Not helpers/serve.ts, for orb_lifecycle.spec.ts's reason: a `bough
// serve` process opens the host's container runtime. The backend is
// TestOrbRemoveDataSafetyBrowserServe (go/tests/model/mbt): the MBT
// adapter's in-process serve on a fake runtime, with a real owner
// process, real git, and the CLI's library calls. One per worker; each
// walk is its own session in its own project.
//
// The page has controls for four actions: Remove… on the orb's row
// (OpenRemovePlan), and the confirm's Cancel and Remove orb (Cancel,
// Confirm, and ConfirmRuntimeRemoveFails with the backend's runtime set
// to fail). Remove… is offered only while the orb is not up; when it is
// not offered, the confirm is opened (and answered) by another client of
// the API, through the backend, and the page shows no dialog. The rest
// (the owner, the agent, project.yml, serve restarting, `bough project
// rm|prune`) has no control on the page and is played by the backend.
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

// The backend binary: $MODEL_ORB_BIN (the mbt test binary, which holds
// both flows' backends), else built once per worker.
function backendBin(worker: number): string {
  if (process.env.MODEL_ORB_BIN) return process.env.MODEL_ORB_BIN;
  const out = path.join(os.tmpdir(), `bough-ords-model-${process.pid}-${worker}.test`);
  execFileSync('go', ['test', '-c', '-o', out, './tests/model/mbt/'], { cwd: goDir, stdio: 'inherit' });
  return out;
}

const test = base.extend<{}, { backend: Backend }>({
  backend: [async ({}, use, info) => {
    const bin = backendBin(info.workerIndex);
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-ords-home-'));
    const urls = path.join(home, 'urls.json');
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home, MODEL_ORDS_SERVE: urls };
    for (const k of Object.keys(env)) if (k.endsWith('_API_KEY')) delete env[k];
    const child: ChildProcess = spawn(bin, ['-test.run', '^TestOrbRemoveDataSafetyBrowserServe$', '-test.v'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
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
  /** The confirm the page has open: its title says whether the plan it showed was dirty. */
  dialog: '' | 'dirty' | 'clean';
  /** Why the page's last remove did not go through, as its alert says it. */
  error: '' | 'dirty' | 'owner' | 'failed';
  /** Console errors the step in flight logs on purpose (a 409 or 500). */
  expected: string[];
}

async function call(b: Backend, route: string): Promise<{ state: State; error?: string; id?: string; slug?: string }> {
  const res = await fetch(b.model + route, { method: 'POST' });
  return res.json();
}

async function must(b: Backend, route: string): Promise<State> {
  const r = await call(b, route);
  if (r.error) throw new Error(`${route}: ${r.error}`);
  return r.state;
}

// --- what the page shows of a state (the spec's fields, plus what the page itself holds) ---

function view(o: State, c: Ctx): State {
  return { row: o.orb === 'present', dialog: c.dialog, error: c.error };
}

// --- readUiState: the same, off the DOM ---

const sessions = (c: Ctx) => c.page.locator('section.orb-sec').filter({ has: c.page.getByRole('heading', { name: 'Sessions', exact: true }) });
// Each walk's project has the one session, so its orb is the only row.
const row = (c: Ctx) => sessions(c).locator('.orb-row');
const dialog = (c: Ctx) => c.page.getByRole('dialog');

async function readUiState(c: Ctx): Promise<State> {
  let dlg = '';
  if (await dialog(c).isVisible()) {
    const title = ((await dialog(c).locator('#dlg-title').textContent()) ?? '').trim();
    dlg = title === 'Uncommitted changes' ? 'dirty' : title === 'Remove this orb?' ? 'clean' : `unknown: ${title}`;
  }
  const alert = sessions(c).getByRole('alert');
  let error = '';
  if (await alert.count()) {
    const t = (await alert.first().textContent()) ?? '';
    error = /uncommitted changes in /.test(t) ? 'dirty'
      : /runs in another bough/.test(t) ? 'owner'
      : /remove failed/.test(t) ? 'failed' : `unknown: ${t}`;
  }
  return { row: (await row(c).count()) > 0, dialog: dlg, error };
}

// --- actions ---

const orbURL = (c: Ctx) => `/api/sessions/${c.id}/orb`;

// The page's own: each is gated on the backend (begin), driven with a
// click, and judged by the backend on the status the page's request got
// (record).
const PAGE: Record<string, (c: Ctx) => Promise<State | null>> = {
  // The GET only shows a plan (the spec's step changes nothing but the
  // flow), so the click goes before the gate: whether Remove… is offered
  // hangs on serve's cached view of the runtime, which the spec does not
  // model, and a button that went while the walk reached for it hands the
  // step to another client instead (null).
  async OpenRemovePlan(c) {
    const got = c.page.waitForResponse((r) => r.url().endsWith(orbURL(c) + '/remove') && r.request().method() === 'GET');
    got.catch(() => {});
    try {
      await row(c).getByRole('button', { name: 'Remove…' }).click({ timeout: 2_000 });
    } catch {
      return null;
    }
    const res = await got;
    await must(c.b, '/model/begin/OpenRemovePlan');
    await expect(dialog(c)).toBeVisible();
    const title = ((await dialog(c).locator('#dlg-title').textContent()) ?? '').trim();
    c.dialog = title === 'Uncommitted changes' ? 'dirty' : 'clean';
    c.error = '';
    return must(c.b, `/model/record?code=${res.status()}`);
  },
  async Cancel(c) {
    await must(c.b, '/model/begin/Cancel');
    await dialog(c).getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(dialog(c)).toBeHidden();
    c.dialog = '';
    return must(c.b, '/model/record?code=200');
  },
  async Confirm(c) { return confirm(c, 'Confirm'); },
  async ConfirmRuntimeRemoveFails(c) { return confirm(c, 'ConfirmRuntimeRemoveFails'); },
};

async function confirm(c: Ctx, name: string): Promise<State> {
  await must(c.b, `/model/begin/${name}`);
  const got = c.page.waitForResponse((r) => r.url().includes(orbURL(c) + '?') && r.request().method() === 'DELETE');
  await dialog(c).getByRole('button', { name: 'Remove orb', exact: true }).click();
  const res = await got;
  c.dialog = '';
  if (res.status() !== 200) c.expected.push(`Failed to load resource: the server responded with a status of ${res.status()} (${res.statusText()})`);
  return must(c.b, `/model/record?code=${res.status()}`);
}

// Which path an action takes: the page's control when the page offers
// it, else the backend (another client of the API, or no client at all).
async function act(c: Ctx, name: string, before: State): Promise<State> {
  switch (name) {
    case 'OpenRemovePlan':
      // The row's one button is Remove… or Stop orb, and a detail read
      // before the last step can swap one for the other under the click
      // (a Stop orb click that was aimed at Remove…, 6 of 708 walks). A
      // fresh load settles which one serve offers now; it also drops the
      // alert of an earlier remove, as any reload would.
      await c.page.reload();
      c.error = '';
      await expect(row(c).locator('.orb-act button')).toBeVisible();
      if (await row(c).getByRole('button', { name: 'Remove…' }).isVisible()) {
        const after = await PAGE.OpenRemovePlan(c);
        if (after) return after;
      }
      break;
    case 'Cancel': case 'Confirm': case 'ConfirmRuntimeRemoveFails':
      if (c.dialog) {
        const after = (await PAGE[name](c))!;
        if (name !== 'Cancel') c.error = refusal(before, after);
        return after;
      }
      break;
  }
  return must(c.b, `/model/act/${name}`);
}

// Why the page's remove did not go through, from the spec's states around
// it: serve's error answers, in removeOrb's order.
function refusal(before: State, after: State): Ctx['error'] {
  if (after.reported) return 'failed';
  if (after.orb !== 'present') return '';
  return before.owner === 'cli' ? 'owner' : 'dirty';
}

// Every node: no sideways scroll, the Sessions section on screen, no
// console errors but the step's own.
async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  for (const e of c.expected.splice(0)) {
    const i = errors.indexOf(e);
    if (i >= 0) errors.splice(i, 1);
  }
  const overflow = await c.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(sessions(c), `${where}: the orb's sessions not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

test.describe('model: orb_remove_data_safety', () => {
  loadPaths('orb_remove_data_safety').forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test(`path ${i}: ${walk}`, async ({ page, backend }) => {
      test.setTimeout(30_000 + 3_000 * trace.length);
      const errors: string[] = [];
      page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e)));
      await page.clock.install();
      // The page never asks for ?branches=1 (api.removeOrb); the spec
      // takes the worst case, a remove that also prunes merged branches,
      // so the page's DELETE is sent as a client that asks for it would.
      await page.route('**/api/sessions/*/orb', (route) => {
        const req = route.request();
        if (req.method() !== 'DELETE') return route.continue();
        return route.continue({ url: req.url() + '?branches=1' });
      });

      const init = await call(backend, '/model/init');
      if (init.error) throw new Error(`Init: ${init.error}`);
      const c: Ctx = { page, b: backend, id: init.id!, slug: init.slug!, dialog: '', error: '', expected: [] };
      try {
        await page.goto(`${backend.ui}/#/projects/${c.slug}/orb`);
        let before: State = init.state;
        for (const [n, step] of trace.entries()) {
          const name = n === 0 ? 'Init' : step.action.slice(ROLE.length + 1);
          const where = `step ${n} (${name})`;
          const want = roleState(ROLE, step.state);
          const got = n === 0 ? init.state : await act(c, name, before);
          expect(got, `${where}: backend state`).toEqual(want);
          await expect.poll(async () => {
            await page.clock.fastForward(5_000);
            return readUiState(c);
          }, { message: `${where}: page`, timeout: 10_000 }).toEqual(view(want, c));
          await invariants(c, errors, where);
          before = want;
        }
      } finally {
        await call(backend, '/model/cleanup');
      }
    });
  });
});
