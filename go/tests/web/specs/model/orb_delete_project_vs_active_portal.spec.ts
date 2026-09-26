// The browser walk of go/tests/model/specs/orb_delete_project_vs_active_portal.fizz:
// a project directory vanishing out from under an open portal, as the
// control room's Portal button shows it. Every generated path is walked
// against a real serve (TestOrbDeleteProjectVsActivePortalBrowserServe,
// go/tests/model/mbt), and at every node the page's own reading of the
// session — the Portal button in the thread header — must equal the
// spec's `recorded` field.
//
// None of the spec's actions has a page control: a project directory
// vanishing and the reaper's sweep are never a button, and neither is
// opening or closing the portal's own listener (that is an orb's agent
// calling its own tool, or the sweep, never a click here) — so every
// action is the same odpAdapter action the Go MBT test
// (orb_delete_project_vs_active_portal_test.go) drives, played through
// the backend's /model/act route. What this walk adds is the page: the
// button must show what that adapter just did, read the same way a
// person looking at the header would, never a value the adapter
// remembers setting.
import { spawn, execFileSync, type ChildProcess } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect, type Page } from '@playwright/test';
import { loadPaths, roleState } from '../../helpers/model';

const ROLE = 'Portal#0';
type State = Record<string, unknown>;

interface Backend { ui: string; model: string; home: string }

const goDir = path.resolve(__dirname, '..', '..', '..', '..');

// The backend binary: $MODEL_ODP_BIN, else built once per worker (go's
// build cache makes every build after the first a link).
function backendBin(worker: number): string {
  if (process.env.MODEL_ODP_BIN) return process.env.MODEL_ODP_BIN;
  const out = path.join(os.tmpdir(), `bough-odp-model-${process.pid}-${worker}.test`);
  execFileSync('go', ['test', '-c', '-o', out, './tests/model/mbt/'], { cwd: goDir, stdio: 'inherit' });
  return out;
}

const test = base.extend<{}, { backend: Backend }>({
  backend: [async ({}, use, info) => {
    const bin = backendBin(info.workerIndex);
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-odp-home-'));
    const urls = path.join(home, 'urls.json');
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home, MODEL_ODP_SERVE: urls };
    for (const k of Object.keys(env)) if (k.endsWith('_API_KEY')) delete env[k];
    const child: ChildProcess = spawn(bin, ['-test.run', '^TestOrbDeleteProjectVsActivePortalBrowserServe$', '-test.v'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    const log: string[] = [];
    child.stdout?.on('data', (d) => log.push(String(d)));
    child.stderr?.on('data', (d) => log.push(String(d)));
    const exited = new Promise<void>((r) => child.once('exit', () => r()));
    const deadline = Date.now() + 30_000;
    while (!fs.existsSync(urls)) {
      if (child.exitCode !== null || Date.now() > deadline) throw new Error(`odp backend did not start:\n${log.join('')}`);
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
      if (!process.env.MODEL_ODP_BIN) fs.rmSync(bin, { force: true });
    }
  }, { scope: 'worker', timeout: 180_000 }],
});

interface Ctx { page: Page; b: Backend; id: string }

async function call(b: Backend, route: string): Promise<{ state: State; error?: string; id?: string }> {
  const res = await fetch(b.model + route, { method: 'POST' });
  return res.json();
}

// --- the spec's own view, line for line (specs/orb_delete_project_vs_active_portal.fizz) ---
// `recorded` is the only field the page shows: the Portal button's label
// comes straight off state.json's Portals list, the same list `recorded`
// reads in the Go adapter. project, listener, opening and serving have no
// page control or readout — they are exercised the same way the Go MBT
// test exercises them, through the backend's own actions.
function view(s: State): State {
  return { recorded: s.recorded };
}

const head = (c: Ctx) => c.page.locator('header.thread-head');
const portalButton = (c: Ctx) => head(c).getByRole('button', { name: /^Portal( — .*)?$/, exact: false });

async function readUiState(c: Ctx): Promise<State> {
  const label = (await portalButton(c).getAttribute('aria-label')) ?? '';
  return { recorded: /is live$/.test(label) };
}

// --- actions: all of the spec's are the adapter's own, played on the backend ---

async function act(c: Ctx, name: string): Promise<State> {
  const r = await call(c.b, `/model/act/${name}`);
  if (r.error) throw new Error(`${name}: ${r.error}`);
  return r.state;
}

async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  const overflow = await c.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(portalButton(c), `${where}: portal status not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

test.describe('model: orb_delete_project_vs_active_portal', () => {
  loadPaths('orb_delete_project_vs_active_portal').forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test(`path ${i}: ${walk}`, async ({ page, backend }) => {
      test.setTimeout(60_000 + trace.length * 16_000);
      const errors: string[] = [];
      page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e)));

      const init = await call(backend, '/model/init');
      if (init.error) throw new Error(`Init: ${init.error}`);
      const c: Ctx = { page, b: backend, id: init.id! };
      try {
        await page.goto(`${backend.ui}/#/s/${c.id}`);
        for (const [n, step] of trace.entries()) {
          const name = n === 0 ? 'Init' : step.action.slice(ROLE.length + 1);
          const where = `step ${n} (${name})`;
          const role = roleState(ROLE, step.state);
          const got = n === 0 ? init.state : await act(c, name);
          expect(got, `${where}: backend state`).toEqual(role);
          // None of this flow's actions produces a session event (no
          // model turn ever runs, see odpHistory), so the page only
          // learns of a change from the open session's own list poll,
          // which slows to POLL_MS*3 (12s) while a session is selected
          // (app.tsx's `streaming`): the timeout has to clear that, not
          // the SSE-driven catch-up other flows' walks can rely on.
          await expect.poll(async () => readUiState(c), {
            message: `${where}: page`, timeout: 15_000,
          }).toEqual(view(role));
          await invariants(c, errors, where);
        }
      } finally {
        await call(backend, '/model/cleanup');
      }
    });
  });
});
