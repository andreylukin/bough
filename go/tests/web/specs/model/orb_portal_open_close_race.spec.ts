// The browser walk of go/tests/model/specs/orb_portal_open_close_race.fizz:
// tools.portal's open/close race against a live orb, as the control room's
// Portal pane shows it. Every generated path in
// testdata/orb_portal_open_close_race/paths.json is walked against a real
// serve (TestOrbPortalOpenCloseRaceBrowserServe, go/tests/model/mbt), and
// at every node the page's own reading of the session — the Portal button
// in the thread header, the pane behind it and the frame it loads — must
// equal the spec's state.
//
// None of the spec's four actions (StopOrb, RestartOrb, Open, Close) has a
// page control: opening or closing a portal is the orb's agent calling its
// own tool over the Host, a restart is the reaper's or another client's
// doing, never a button — so every action here is the same portalAdapter
// action the Go MBT test (orb_portal_open_close_race_test.go) drives,
// played through the backend's /model/act route, exactly as
// orb_lifecycle.spec.ts plays the child's own writes. What this walk adds
// is the page: it must show what that adapter just did, without being told
// anything beyond what a person watching the pane would see.
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

// The backend binary: $MODEL_PORTAL_BIN, else built once per worker (go's
// build cache makes every build after the first a link).
function backendBin(worker: number): string {
  if (process.env.MODEL_PORTAL_BIN) return process.env.MODEL_PORTAL_BIN;
  const out = path.join(os.tmpdir(), `bough-portal-model-${process.pid}-${worker}.test`);
  execFileSync('go', ['test', '-c', '-o', out, './tests/model/mbt/'], { cwd: goDir, stdio: 'inherit' });
  return out;
}

const test = base.extend<{}, { backend: Backend }>({
  backend: [async ({}, use, info) => {
    const bin = backendBin(info.workerIndex);
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-portal-home-'));
    const urls = path.join(home, 'urls.json');
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home, MODEL_PORTAL_SERVE: urls };
    for (const k of Object.keys(env)) if (k.endsWith('_API_KEY')) delete env[k];
    const child: ChildProcess = spawn(bin, ['-test.run', '^TestOrbPortalOpenCloseRaceBrowserServe$', '-test.v'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    const log: string[] = [];
    child.stdout?.on('data', (d) => log.push(String(d)));
    child.stderr?.on('data', (d) => log.push(String(d)));
    const exited = new Promise<void>((r) => child.once('exit', () => r()));
    const deadline = Date.now() + 30_000;
    while (!fs.existsSync(urls)) {
      if (child.exitCode !== null || Date.now() > deadline) throw new Error(`portal backend did not start:\n${log.join('')}`);
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
      if (!process.env.MODEL_PORTAL_BIN) fs.rmSync(bin, { force: true });
    }
  }, { scope: 'worker', timeout: 180_000 }],
});

interface Ctx { page: Page; b: Backend; id: string }

async function call(b: Backend, route: string): Promise<{ state: State; error?: string; id?: string }> {
  const res = await fetch(b.model + route, { method: 'POST' });
  return res.json();
}

// --- the spec's own view, line for line (specs/orb_portal_open_close_race.fizz) ---
// The spec's fields translate straight onto the page: orbRunning is
// whether Stop orb is offered (RuntimeStrip, web/src/app.tsx's orbUp),
// open is whether the Portal button says a port is live (PortalButton;
// row.orb.portals drops dead records, so any entry means open), and dial
// is which of the two fake backends actually answers through the portal's
// own frame — read the same way a person looking at the pane would.
function view(s: State): State {
  return { orbRunning: s.orbRunning, orbIP: s.orbIP, open: s.open, dial: s.dial };
}

const head = (c: Ctx) => c.page.locator('header.thread-head');
const portalButton = (c: Ctx) => head(c).getByRole('button', { name: /^Portal( — .*)?$/, exact: false });

async function readUiState(c: Ctx): Promise<State> {
  const h = head(c);
  const orbRunning = await h.getByRole('button', { name: 'Stop orb', exact: true }).isVisible();
  const label = (await portalButton(c).getAttribute('aria-label')) ?? '';
  const open = /is live$|are live$/.test(label);
  let orbIP = '';
  let dial = '';
  if (open) {
    // Open the pane if it is not already showing, then read what its
    // frame actually loaded: the fake backend's own body text ("ip1" or
    // "ip2"), the real dial a user looking at the page would see.
    await portalButton(c).click();
    const frame = c.page.frameLocator('.portal-frame');
    const text = await frame.locator('body').innerText({ timeout: 5000 }).catch(() => '');
    const t = text.trim();
    if (t === 'ip1' || t === 'ip2') { dial = t; orbIP = t; }
  }
  return { orbRunning, open, orbIP, dial };
}

// --- actions: all four are the adapter's own, played on the backend ---

async function act(c: Ctx, name: string): Promise<State> {
  const r = await call(c.b, `/model/act/${name}`);
  if (r.error) throw new Error(`${name}: ${r.error}`);
  return r.state;
}

const actions: Record<string, (c: Ctx) => Promise<void>> = {
  StopOrb: (c) => act(c, 'StopOrb').then(() => undefined),
  RestartOrb: (c) => act(c, 'RestartOrb').then(() => undefined),
  Open: (c) => act(c, 'Open').then(() => undefined),
  Close: (c) => act(c, 'Close').then(() => undefined),
};

async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  const overflow = await c.page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(portalButton(c), `${where}: portal status not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

// QUARANTINED: the backend relabels an ordinary CreateSession'd session
// as "project" (markProjectSession) so the page renders a Portal pane
// for it, but CreateSession also starts a real local child regardless
// of the empty prompt. internal/serve/orbs.go's ownerAlive then compares
// portalState's PID against THAT live child's, not this backend
// process's, and forces the fake orb to "stopped" on every real
// /api/sessions/{id} the page fetches — Stop (the Work panel's
// interrupt) is a no-op on an idle no-task session, so there is no
// servetest-exposed way from this package to actually end that child
// and clear serve's kids-map entry for it. Needs either an exported way
// to kill a session's local child, or a session creation path that
// never starts one, before this can run for real.
test.describe('model: orb_portal_open_close_race', () => {
  loadPaths('orb_portal_open_close_race').forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test.fixme(`path ${i}: ${walk} [quarantined: fake orb forced to "stopped" by ownerAlive against the real local child CreateSession starts]`, async ({ page, backend }) => {
      test.setTimeout(60_000 + trace.length * 3_000);
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
          await expect.poll(async () => readUiState(c), {
            message: `${where}: page`, timeout: 10_000,
          }).toEqual(view(role));
          await invariants(c, errors, where);
        }
      } finally {
        await call(backend, '/model/cleanup');
      }
    });
  });
});
