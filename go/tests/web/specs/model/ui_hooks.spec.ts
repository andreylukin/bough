// go/tests/model/specs/ui_hooks.fizz walked in the browser (recipe:
// go/tests/model/README.md): the Hooks page as the browser holds it —
// the list's loading/slow/error/stale, one hook's definition panel, its
// text, Save and Dry run, and Back. Every generated path is one test on
// a serve shared by the worker; at every node readUiState must equal the
// spec's Page#0 state, and the surface must pass the checks every screen
// owes: no sideways scroll, no clipped text in the status or headers, no
// console errors, a visible ring on whatever holds focus, and axe clean.
//
// The page decides nothing about when its requests answer, so the walk
// does: every hooks request is held at the network until the spec's
// answer for it (Poll, Loaded, SaveOk, DryFail, ...) lets it through to
// serve or fails it. Controls are driven from the keyboard (Enter on a
// focused button, typing in the textarea), the way a keyboard user
// would, so :focus-visible is in force and the ring check means
// something (a mouse click rightly draws no ring, so it would prove less).
//
// This walker is the shape of helpers/model.ts's modelTests with those
// extra per-node checks; the shared helper is left as every flow has it.
import * as fs from 'fs';
import * as path from 'path';
import AxeBuilder from '@axe-core/playwright';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG } from '../../helpers/control';
import { loadPaths, roleState } from '../../helpers/model';
import { expect, test, type Serve } from '../../helpers/serve';

const SPEC = 'ui_hooks';
const ROLE = 'Page#0';
const EVENT = 'pre-code-exec';
const NAME = 'guard.js';
const BODY = '// guard: lets every block run\nreturn {};\n';
// Same as hooks.tsx POLL_MS: moving the page's clock this far makes the
// list's interval ask again.
const POLL_MS = 5_000;
// Pending's "taking too long" (loading.tsx, the list's default timeout).
const LIST_TIMEOUT_MS = 20_000;

interface Ctx {
  page: Page;
  serve: Serve;
  hook: string;       // guard.js on disk
  lists: Route[];     // held GET /api/hooks
  files: Route[];     // held GET /api/hooks/file
  saves: Route[];     // held PUT /api/hooks/file
  dries: Route[];     // held POST /api/hooks/dryrun
  edits: number;
}

// A failed request the page cannot tell from any other. A 5xx would do
// the same to the page, but Chromium logs every non-2xx fetch as a
// console error, which the per-node check forbids; a 200 whose body is
// not JSON fails the same req() call quietly. The page shows a save's
// failure as JSON.parse words it ("Unexpected token 'S', "Save failed"
// is not valid JSON"), so the body's first word says whose it is. A dry
// run fails the way a throwing hook does: serve answers with its error.
const SAVE_FAIL = 'Save failed';
const DRY_FAIL = 'Dry run failed';
const failWith = (r: Route, what: string) =>
  r.fulfill({ status: 200, contentType: 'application/json', body: what });
const dryFail = (r: Route) =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ result: null, error: DRY_FAIL, ms: 0 }) });
const saveFailed = (err: string) => err.includes('"Save');

const row = (c: Ctx) =>
  c.page.locator('.hk2-row').filter({ has: c.page.locator('.hk2-name', { hasText: new RegExp(`^${NAME.replace('.', '\\.')}$`) }) });
const panel = (c: Ctx) => row(c).locator('.hk-panel');
const toggle = (c: Ctx) => row(c).locator('.hk-toggle');
const saveBtn = (c: Ctx) => panel(c).locator('.hk-acts .btn-primary');
const dryBtn = (c: Ctx) => panel(c).locator('.hk-acts .btn:not(.btn-primary)');
const onHooks = (c: Ctx) => c.page.locator('.thread:has(.page-head h1:text-is("Hooks"))');

// Wait until a request of the kind is held; a list read only goes out on
// the poll's interval, so the page's clock is moved until one is.
async function held(c: Ctx, q: Route[], tick: boolean): Promise<void> {
  const deadline = Date.now() + 10_000;
  while (q.length === 0) {
    if (Date.now() > deadline) throw new Error(`${SPEC}: no request to answer`);
    if (tick) await c.page.clock.fastForward(POLL_MS);
    await new Promise((r) => setTimeout(r, 25));
  }
}
// Answer every held request of a kind: a second click (Dry run twice, or
// Show again while a read was out) sent a second one, and the spec's one
// answer stands for all of them.
async function answer(q: Route[], fn: (r: Route) => Promise<void>): Promise<void> {
  for (const r of q.splice(0)) await fn(r);
}

// Enter on a control, as a keyboard user presses it.
async function press(l: ReturnType<Page['locator']>): Promise<void> {
  await l.focus();
  await l.press('Enter');
}

const AWAY = { route: 'away', list: 'loading', panel: 'closed', file: 'none', edited: false, save: 'idle', dry: 'idle' };

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const hash = await page.evaluate(() => window.location.hash);
  if (hash !== '#/hooks') {
    // HooksPage is unmounted: nothing of it may be left on screen, and
    // the nav must not claim the page.
    const left = await onHooks(c).count();
    const current = await page.locator('.side-nav a[href="#/hooks"][aria-current="page"]').count();
    return left || current ? { ...AWAY, route: `away, but hooks still shown (${left}/${current})` } : AWAY;
  }
  const body = page.locator('.proj-body').first();
  let list = 'unknown';
  if (await body.locator('.hk-stale').count()) list = 'stale';
  else if (await body.locator('.hk-chips').count()) list = 'loaded';
  else if (await body.locator('> .skeleton[role=status]').count()) list = 'loading';
  else {
    const title = (await body.locator('.error-note-title').allTextContents())[0] ?? '';
    if (title === 'Hooks is taking too long') list = 'slow';
    else if (title === 'Couldn’t load hooks') list = 'error';
  }

  let panelState = 'closed';
  let file = 'none';
  let edited = false;
  let save = 'idle';
  let dry = 'idle';
  if (await row(c).count()) {
    panelState = (await toggle(c).getAttribute('aria-expanded')) === 'true' ? 'open' : 'closed';
    const p = panel(c);
    const text = p.locator('textarea.hk-edit');
    if (await text.count()) {
      file = 'loaded';
      // What was last read or saved is what is on disk: Save writes it,
      // Loaded read it.
      edited = (await text.inputValue()) !== fs.readFileSync(c.hook, 'utf8');
      const note = (await p.locator('.hk-note[role=status]').allTextContents())[0] ?? '';
      const err = (await p.locator(':scope > p.err[role=alert]').allTextContents())[0] ?? '';
      if ((await saveBtn(c).textContent()) === 'Saving…') save = 'saving';
      else if (note === 'Saved.') save = 'saved';
      else if (saveFailed(err)) save = 'error';
      if ((await dryBtn(c).getAttribute('aria-busy')) === 'true') dry = 'running';
      else if (note.startsWith('Dry run finished')) dry = 'result';
      else if (err.includes(DRY_FAIL)) dry = 'error';
      if (err && !saveFailed(err) && !err.includes(DRY_FAIL)) save = `unexpected error: ${err}`;
    } else if (await p.locator('.hk-loaderr').count()) file = 'error';
    else if (await p.locator('[role=status]').count()) file = 'loading';
  }
  return { route: 'hooks', list, panel: panelState, file, edited, save, dry };
}

const actions: Record<string, (c: Ctx) => Promise<void>> = {
  Poll: async (c) => { await held(c, c.lists, true); await answer(c.lists, (r) => r.continue()); },
  PollFail: async (c) => { await held(c, c.lists, true); await answer(c.lists, (r) => failWith(r, 'List read failed')); },
  // Pending gives up after 20 s of the page's clock. The list request is
  // still held, so the interval's refresh() finds it in flight and sends
  // nothing new.
  ListTimeout: (c) => c.page.clock.fastForward(LIST_TIMEOUT_MS),
  ListRetry: (c) => press(c.page.locator('.proj-body .error-note').getByRole('button', { name: 'Retry' })),

  Show: (c) => press(toggle(c)),
  Hide: (c) => press(toggle(c)),
  Loaded: async (c) => { await held(c, c.files, false); await answer(c.files, (r) => r.continue()); },
  LoadFail: async (c) => { await held(c, c.files, false); await answer(c.files, (r) => failWith(r, 'File read failed')); },
  FileRetry: (c) => press(panel(c).locator('.hk-loaderr').getByRole('button', { name: 'Retry' })),

  async Edit(c) {
    const text = panel(c).locator('textarea.hk-edit');
    await text.focus();
    await c.page.keyboard.press('ControlOrMeta+End');
    await c.page.keyboard.type(`\n// edit ${++c.edits}`);
  },
  Save: async (c) => { await press(saveBtn(c)); await held(c, c.saves, false); },
  SaveOk: (c) => answer(c.saves, (r) => r.continue()),
  SaveFail: (c) => answer(c.saves, (r) => failWith(r, SAVE_FAIL)),
  DryRun: async (c) => {
    const before = c.dries.length;
    await press(dryBtn(c));
    const deadline = Date.now() + 10_000;
    while (c.dries.length === before && Date.now() < deadline) await new Promise((r) => setTimeout(r, 25));
  },
  DryOk: (c) => answer(c.dries, (r) => r.continue()),
  DryFail: (c) => answer(c.dries, dryFail),

  // The header's Back button exists only in the phone layout; at this
  // width leaving is another nav item, which unmounts the page the same.
  async Back(c) {
    await press(c.page.locator('.side-nav a[href="#/projects"]').first());
    await expect(c.page).not.toHaveURL(/#\/hooks$/);
    // Reads still out belong to the page that just unmounted; answering
    // them now keeps them from standing in for the next mount's.
    await answer(c.lists, (r) => failWith(r, 'Unmounted'));
    await answer(c.files, (r) => failWith(r, 'Unmounted'));
  },
  Return: (c) => press(c.page.locator('.side-nav a[href="#/hooks"]').first()),
};

// ---- the checks every node owes ----

// Text in the state's carriers and the headers must fit its box: a
// clipped "Saving…" or error title says less than the page means to.
const CLIP_SELECTORS = [
  '.page-head h1', '.hk-stale', '.error-note-title', '.error-note-body', '.hk2-name',
  '.hk-toggle', '.hk-acts .btn', '.hk-note', '.hk-panel > p.err', '.hk-loaderr', '.hk-chip',
].join(', ');

async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  const { page } = c;
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);

  const clipped = await page.evaluate((sel) => {
    const out: string[] = [];
    for (const el of Array.from(document.querySelectorAll<HTMLElement>(sel))) {
      if (!el.offsetParent && getComputedStyle(el).position !== 'fixed') continue; // not rendered
      const cs = getComputedStyle(el);
      const hides = (v: string) => v !== 'visible';
      const w = hides(cs.overflowX) && el.scrollWidth > el.clientWidth + 1;
      const h = hides(cs.overflowY) && el.scrollHeight > el.clientHeight + 1;
      if (w || h) out.push(`${el.className || el.tagName}: "${(el.textContent ?? '').trim().slice(0, 40)}"`);
    }
    return out;
  }, CLIP_SELECTORS);
  expect(clipped, `${where}: clipped text`).toEqual([]);

  // The carrier: the hooks surface while on the page, the nav item that
  // brings it back while away.
  const carrier = (await page.evaluate(() => window.location.hash)) === '#/hooks'
    ? page.locator('.proj-body').first()
    : page.locator('.side-nav a[href="#/hooks"]').first();
  await expect(carrier, `${where}: status not visible`).toBeVisible();

  const ring = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body || el === document.documentElement) return null;
    const cs = getComputedStyle(el);
    const outline = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0;
    const shadow = cs.boxShadow !== 'none';
    return {
      what: `${el.tagName.toLowerCase()}.${el.className} "${(el.textContent ?? '').trim().slice(0, 30)}"`,
      visible: el.getClientRects().length > 0,
      focusVisible: el.matches(':focus-visible'),
      ring: outline || shadow,
    };
  });
  if (ring) expect(ring, `${where}: focus ring on ${ring.what}`).toMatchObject({ visible: true, focusVisible: true, ring: true });

  expect(errors, `${where}: console errors`).toEqual([]);

  if (await onHooks(c).count()) {
    // A state fades in (mb-state-in); axe measuring contrast mid-fade
    // reports text that is only faint for 200 ms. Endless spinners are
    // left running.
    await page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => a.effect?.getTiming().iterations !== Infinity)
      .map((a) => a.finished.catch(() => undefined))));
    const axe = await new AxeBuilder({ page }).include('.thread').options({ resultTypes: ['violations'] }).analyze();
    const found = axe.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`);
    expect(found, `${where}: axe`).toEqual([]);
  }
}

// One serve per worker: a path starts by putting guard.js back and
// loading the page afresh, which is all of the spec's Init.
test.use({ workerServeOpts: { config: CONTROL_CONFIG } });

test.describe(`model: ${SPEC}`, () => {
  // axe is most of a node's cost (~0.7 s); the longest path has 11 nodes.
  test.describe.configure({ timeout: 90_000 });
  loadPaths(SPEC).forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test(`path ${i}: ${walk}`, async ({ sharedServe: serve, page }) => {
      const hook = path.join(serve.home, '.bough', 'hooks', EVENT, NAME);
      fs.mkdirSync(path.dirname(hook), { recursive: true });
      fs.writeFileSync(hook, BODY);
      const c: Ctx = { page, serve, hook, lists: [], files: [], saves: [], dries: [], edits: 0 };

      const errors: string[] = [];
      page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e)));

      await page.route((u) => u.pathname === '/api/hooks', (r) => { c.lists.push(r); });
      await page.route((u) => u.pathname === '/api/hooks/file', (r) => {
        if (r.request().method() === 'GET') c.files.push(r);
        else c.saves.push(r);
      });
      await page.route((u) => u.pathname === '/api/hooks/dryrun', (r) => { c.dries.push(r); });
      await page.clock.install();
      await page.goto(`${serve.url}/#/hooks`);

      try {
        for (const [n, step] of trace.entries()) {
          const name = step.action === 'Init' ? 'Init' : step.action.slice(ROLE.length + 1);
          const where = `step ${n} (${name})`;
          if (n > 0) {
            const act = actions[name];
            if (!act) throw new Error(`${SPEC}: no action for ${step.action}`);
            await act(c);
          }
          // No clock moves here: the spec has the poll, the list's
          // timeout and every answer as actions of their own.
          await expect.poll(() => readUiState(c), { message: `${where}: state`, timeout: 10_000 })
            .toEqual(roleState(ROLE, step.state));
          await invariants(c, errors, where);
        }
      } finally {
        await page.unrouteAll({ behavior: 'ignoreErrors' });
      }
    });
  });
});
