// go/tests/model/specs/setup_welcome.fizz in the browser: the first-run
// welcome on a phone, against a real serve per path (recipe:
// go/tests/model/README.md).
//
// Why not the worker-scoped serve: a saved key lives in serve's process
// env and nothing unsets it, and the welcome only shows itself on a
// server with no sessions, so every path's Init needs a server nobody
// has touched.
//
// The requests the spec has a state for while they are in flight (the
// key save, the key check, the folder check, the start) are held at the
// page's network layer and let through by the action that ends them; the
// server answers every one of them for real. The key check goes to a
// local stub provider (BOUGH_SETUP_CHECK_URL) answering as the Key*
// action says, and a proxy that goes nowhere catches any check that
// would still leave the machine.
import * as fs from 'fs';
import * as http from 'http';
import type { AddressInfo } from 'net';
import * as path from 'path';
import type { Page, Request, Route } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

// The provider stub, one per worker (tests in a worker run one at a
// time): every check is answered with `answer`.
let answer = 200;
const stub: Promise<string> = new Promise((resolve) => {
  const srv = http.createServer((_req, res) => { res.statusCode = answer; res.end(); });
  srv.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${(srv.address() as AddressInfo).port}`));
  srv.unref();
});

test.use({
  viewport: { width: 390, height: 844 },
  serveOpts: async ({}, use) => {
    await use({
      cwd: 'start',
      // repo is a checkout (a .git dir), start is a plain folder.
      home: { 'repo/.git/HEAD': 'ref: refs/heads/main\n', 'start/.keep': '' },
      env: {
        BOUGH_SETUP_CHECK_URL: await stub,
        // Loopback is never proxied: only a check that slipped past the
        // stub goes here, and fails instead of going out.
        HTTPS_PROXY: 'http://127.0.0.1:9', HTTP_PROXY: 'http://127.0.0.1:9',
      },
    });
  },
});

type Kind = 'save' | 'check' | 'folder' | 'start';

interface Ctx {
  page: Page;
  serve: Serve;
  held: Record<Kind, Route[]>;
  saves: number;
  sessions: string[];
  // What the page knows but does not show on every screen, each set from
  // the page's own traffic or the person's own act (see readUiState).
  keyset: boolean;
  path: string;             // the field's text as last typed (or reset on mount)
  answered: string | null;  // the folder answer for that path, null while asked
  start: string;
  skipped: boolean;
}

const PATHS = { plain: '~/start', checkout: '~/repo', missing: '~/nowhere' } as const;
const typedOf = (p: string) => p.endsWith('/start') ? 'plain' : p.endsWith('/repo') ? 'checkout' : p.endsWith('/nowhere') ? 'missing' : `? ${p}`;

// A request the spec says is in flight: wait for the page to send it
// (the folder check waits out a 250 ms debounce on the page's clock).
async function take(c: Ctx, kind: Kind): Promise<Route> {
  for (let i = 0; i < 100; i++) {
    const r = c.held[kind].shift();
    if (r) return r;
    await c.page.clock.fastForward(100);
    await c.page.waitForTimeout(20);
  }
  throw new Error(`the page never sent its ${kind} request`);
}

// The welcome unmounted: what it still had in flight is dropped by the
// page, so it is left hanging here (a save let through would set a key
// the spec says was never saved).
function abandon(c: Ctx): void {
  for (const k of Object.keys(c.held) as Kind[]) c.held[k] = [];
}

const welcome = (c: Ctx) => c.page.locator('.welcome');
const steps = (c: Ctx) => c.page.locator('.welcome-steps > li');
const folderField = (c: Ctx) => c.page.getByLabel('Folder', { exact: true });

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const shown = await welcome(c).isVisible();
  const done = await page.evaluate(() => { try { return localStorage.getItem('bough:welcome-done') === '1'; } catch { return false; } });
  const session = (await page.locator('button.row').count()) > 0;
  const pane = await page.locator('.app').getAttribute('data-pane');
  // The page keeps "auto" and "on" apart only in memory. They show the
  // same screen; "on" is only ever the palette's, after a Skip wrote
  // bough:welcome-done.
  const mode = shown ? (done ? 'on' : 'auto') : session ? (c.skipped ? 'on' : 'auto') : 'off';
  const s: Record<string, unknown> = {
    welcome: mode, done, skipped: c.skipped, session, pane, keyset: c.keyset,
    key: '', typed: '', folder: '', start: 'idle',
  };
  if (!shown) return s;

  // Step 1: the key line.
  const one = steps(c).nth(0);
  const cls = (await one.getAttribute('class')) ?? '';
  if (await one.getByRole('button', { name: 'Saving…' }).isVisible()) s.key = 'saving';
  else if (cls.includes(' done')) s.key = 'works';
  else if (cls.includes(' rejected')) s.key = 'rejected';
  else if (await one.getByText('Checking your keys…').isVisible()) s.key = 'checking';
  else if (await one.getByRole('button', { name: 'Save key' }).isVisible()) s.key = 'unset';
  else s.key = `? ${await one.innerText()}`;

  // Step 2: the field and the line under it; once a checkout is found the
  // step folds to its path. Until a key works the step is closed and the
  // page shows neither: then it is the path it was given and the answer
  // it got.
  const two = steps(c).nth(1);
  if (await folderField(c).isVisible()) {
    s.typed = typedOf(await folderField(c).inputValue());
    const line = await two.innerText();
    s.folder = line.includes('Checking folder…') ? 'checking'
      : line.includes('No folder at this path.') ? 'missing'
      : line.includes('Not a git checkout.') ? 'plain'
      : line.includes('Git checkout found') ? 'checkout' : `? ${line}`;
  } else if ((await two.getAttribute('class'))?.includes(' done')) {
    s.typed = typedOf(await two.locator('.welcome-sum').innerText());
    s.folder = 'checkout';
  } else {
    s.typed = typedOf(c.path);
    s.folder = c.answered ?? 'checking';
  }

  // Step 3: open only while a key works and the folder exists.
  const three = steps(c).nth(2);
  if (await three.locator('.welcome-chips').isVisible()) {
    s.start = (await three.getByRole('button', { name: 'Starting…' }).isVisible()) ? 'starting'
      : (await three.getByText('Couldn’t start the session.').isVisible()) ? 'failed' : 'idle';
  } else s.start = c.start;
  return s;
}

function mounted(c: Ctx): void {
  c.path = PATHS.plain;
  c.answered = null;
  c.start = 'idle';
}

async function check(c: Ctx, status: number): Promise<void> {
  const r = await take(c, 'check');
  answer = status;
  await r.continue();
}

async function edit(c: Ctx, typed: keyof typeof PATHS): Promise<void> {
  if (!(await folderField(c).isVisible())) await steps(c).nth(1).getByRole('button', { name: 'Change' }).click();
  c.path = PATHS[typed];
  c.answered = null;
  await folderField(c).fill(c.path);
}

// The page's own knowledge of whether a key is set: the provider list in
// the last /api/setup or /api/setup/key answer it read.
function watchProviders(c: Ctx): void {
  c.page.on('response', async (res) => {
    const u = new URL(res.url());
    if (u.pathname !== '/api/setup' && u.pathname !== '/api/setup/key') return;
    if (!res.ok()) return;
    try {
      const body = await res.json() as { providers?: { name: string; set: boolean }[] };
      const p = body.providers?.find((x) => x.name === 'anthropic');
      if (p) c.keyset = p.set;
    } catch { /* page closed */ }
  });
}

modelTests<Ctx>({
  spec: 'setup_welcome',
  role: 'Setup#0',

  async init(page, serve) {
    const c: Ctx = {
      page, serve, held: { save: [], check: [], folder: [], start: [] }, saves: 0, sessions: [],
      keyset: false, path: PATHS.plain, answered: null, start: 'idle', skipped: false,
    };
    const hold = (k: Kind) => (r: Route) => { c.held[k].push(r); };
    await page.route((u) => u.pathname === '/api/setup/key', hold('save'));
    await page.route((u) => u.pathname === '/api/setup/check', hold('check'));
    await page.route((u) => u.pathname === '/api/setup' && u.searchParams.has('cwd'), hold('folder'));
    await page.route((u) => u.pathname === '/api/sessions', (r) => r.request().method() === 'POST' ? hold('start')(r) : r.fallback());
    watchProviders(c);
    await page.goto(serve.url);
    return c;
  },

  actions: {
    async SaveKey(c) {
      await c.page.getByLabel('Anthropic API key').fill(`sk-ant-${String(++c.saves).padStart(4, '0')}`);
      await c.page.getByRole('button', { name: 'Save key' }).click();
    },
    async SaveOk(c) {
      const r = await take(c, 'save');
      const res = c.page.waitForResponse((x) => x.request() === r.request());
      await r.continue();
      await res;
    },
    // A key the server refuses to write (a pasted newline): a password
    // field cannot hold one, so the held request carries it instead.
    async SaveFail(c) {
      const r = await take(c, 'save');
      const res = c.page.waitForResponse((x) => x.request() === r.request());
      await r.continue({ postData: JSON.stringify({ provider: 'anthropic', key: 'sk-ant\nbroken' }) });
      if ((await res).ok()) throw new Error('a key with a newline in it: the server saved it');
    },
    KeyOk: (c) => check(c, 200),
    KeyRejected: (c) => check(c, 401),
    // A provider that answers but not about the key: serve says "unknown".
    KeyUnknown: (c) => check(c, 502),
    EditMissing: (c) => edit(c, 'missing'),
    EditPlain: (c) => edit(c, 'plain'),
    EditCheckout: (c) => edit(c, 'checkout'),
    async FolderAnswer(c) {
      const r = await take(c, 'folder');
      const res = c.page.waitForResponse((x) => x.request() === r.request());
      await r.continue();
      const f = (await (await res).json()).folder as { exists: boolean; checkout?: string };
      c.answered = !f.exists ? 'missing' : f.checkout ? 'checkout' : 'plain';
    },
    async Start(c) {
      await c.page.getByRole('button', { name: 'Explain this repo' }).click();
      c.start = 'starting';
    },
    async StartOk(c) {
      const r = await take(c, 'start');
      const res = c.page.waitForResponse((x) => x.request() === r.request());
      await r.continue();
      c.sessions.push((await (await res).json()).session.id);
      c.start = 'idle';
    },
    // The folder the start was for goes away between its check and the
    // start, so the server refuses it; it comes back at once, so the
    // line the page still shows is true again.
    async StartFail(c) {
      const r = await take(c, 'start');
      const cwd = (r.request().postDataJSON() as { cwd: string }).cwd;
      const aside = cwd + '.aside';
      fs.renameSync(cwd, aside);
      try {
        const res = c.page.waitForResponse((x) => x.request() === r.request());
        await r.continue();
        if ((await res).ok()) throw new Error(`a start in ${path.basename(cwd)}, which is gone: the server started it`);
      } finally {
        fs.renameSync(aside, cwd);
      }
      c.start = 'failed';
    },
    async Skip(c) {
      await c.page.getByRole('button', { name: 'Skip the welcome' }).click();
      c.skipped = true;
      abandon(c);
    },
    async ShowWelcome(c) {
      await c.page.keyboard.press(process.platform === 'darwin' ? 'Meta+k' : 'Control+k');
      await c.page.getByRole('combobox').fill('welcome');
      await c.page.getByRole('option', { name: /Show the welcome/ }).click();
      mounted(c);
    },
  },

  // SaveFail's 500 and StartFail's 400 are the refusals those actions ask
  // the server for.
  expectedErrors: [/^Failed to load resource: the server responded with a status of (400|500) /],

  read: readUiState,
  // What carries the state: the welcome while it is up, else the pane
  // the phone is on.
  status: (c) => c.page.locator('.welcome-setup, .app[data-pane="list"] .sidebar, .app[data-pane="thread"] #composer').first(),
  sessions: (c) => c.sessions,
});
