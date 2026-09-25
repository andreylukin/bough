// go/tests/model/specs/narrow_layout.fizz in the browser: the control
// room at phone width (390x844), one pane at a time. Every generated
// path is one test against a real serve (recipe: go/tests/model/README.md);
// at each node readUiState must equal the spec's Phone#0 state.
//
// Why not the worker-scoped serve: the spec starts on an empty server
// (the welcome), and archived sessions, a started welcome and a filed
// project all outlive a test, so every path's Init needs a server nobody
// has touched.
//
// Three things a phone has that a desktop Chromium does not, each
// supplied by an init script and read back from what the page did:
//
//   - the on-screen keyboard: window.visualViewport is replaced by one
//     the walk can shrink (KeyboardUp). Like the OS, it goes down when
//     focus leaves something that takes typing, or that element leaves
//     the page. `keyboard` is read off the page's own --app-height.
//   - "fitted": the open overlay's bottom is inside the visual viewport
//     with the keyboard up. With the keyboard down it is probed: raised
//     and lowered inside one evaluate, so the page's resize handlers run
//     but nothing renders in between.
//   - acked_on_list: every POST .../ack the page sends is logged with the
//     pane it was sent from, in sessionStorage so a reload keeps it.
//
// The page acks a finish on its own the moment it is on screen, and the
// spec takes that as its own step (Ack). So from a Finish until the Ack
// step the page's acks are held at the network; Ack lets them through.
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

const SLUG = 'demo';
// An iPhone keyboard's share of an 844px screen.
const KB = 320;

test.use({
  viewport: { width: 390, height: 844 },
  serveOpts: {
    config: CONTROL_CONFIG,
    // The welcome's Start wants a working key. It is never used (the
    // model is llm-control) and never checked: the page's key check is
    // answered below, before it reaches serve.
    home: { '.bough/env': 'ANTHROPIC_API_KEY=sk-test-not-a-key\n' },
  },
});

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;        // the walk's session, '' until the welcome or another tab makes it
  turn: number;
  held: string[];    // llm-control turns still blocked (the Work sheet's agent)
  holdAcks: boolean; // a finish is on the server that the spec has not acked yet
  acks: Route[];
  // "The last move was Back": a fact about the walk, not a screen. The
  // screen half of the claim (the list, #/) is read off the page.
  backed: boolean;
}

const phone = () => {
  const w = window as unknown as Record<string, unknown>;
  let kb = 0;
  const vv = new EventTarget();
  Object.defineProperties(vv, {
    width: { get: () => innerWidth },
    height: { get: () => innerHeight - kb },
    offsetTop: { get: () => 0 },
    offsetLeft: { get: () => 0 },
    pageTop: { get: () => scrollY },
    pageLeft: { get: () => scrollX },
    scale: { get: () => 1 },
  });
  Object.defineProperty(window, 'visualViewport', { value: vv, configurable: true });
  const set = (px: number) => { if (kb !== px) { kb = px; vv.dispatchEvent(new Event('resize')); } };
  const typing = (el: Element | null) => Boolean(el && ((el as HTMLElement).isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)));
  const follow = () => { if (kb && !typing(document.activeElement)) set(0); };
  document.addEventListener('focusin', follow, true);
  document.addEventListener('focusout', (e) => { if (!(e as FocusEvent).relatedTarget) queueMicrotask(follow); }, true);
  new MutationObserver(follow).observe(document, { childList: true, subtree: true });
  w.__osk = { set, kb: () => kb };

  const KEY = 'narrow-acks';
  const f = window.fetch;
  window.fetch = function (this: unknown, input: RequestInfo | URL, init?: RequestInit) {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    if (/\/api\/sessions\/[^/]+\/ack$/.test(url)) {
      const pane = document.querySelector('.app')?.getAttribute('data-pane') ?? '?';
      try { sessionStorage.setItem(KEY, (sessionStorage.getItem(KEY) ?? '') + pane + ','); } catch { /* the read says so */ }
    }
    return f.call(this, input, init);
  } as typeof fetch;
};

const OVERLAY = { settings: '.head-pop', work: '.work-sheet', threads: '.prj-threads[data-open]', panel: '.prj-panel' } as const;

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const hash = await page.evaluate(() => location.hash);
  const view = /^#\/projects\/[^/]+(\/t\/.*)?$/.test(hash) ? 'project'
    : /^#\/(me|projects|hooks|wiki)(\/|$)/.test(hash) ? 'page' : 'sessions';
  const selected = view === 'project'
    ? await page.locator('.prj-crumb .prj-back').isVisible()
    : hash.startsWith('#/s/');
  let overlay = 'none';
  for (const [k, sel] of Object.entries(OVERLAY)) if (await page.locator(sel).isVisible()) overlay = k;
  const label = c.id ? (await page.locator(`button.row[data-id="${c.id}"]`).first().getAttribute('aria-label').catch(() => '')) ?? '' : '';
  const dom = await page.evaluate((sel) => {
    const osk = (window as unknown as { __osk: { set(px: number): void; kb(): number } }).__osk;
    const appHeight = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--app-height'));
    const keyboard = appHeight < innerHeight - 1;
    let fitted = true;
    const el = sel ? document.querySelector(sel) : null;
    if (el) {
      const was = osk.kb();
      if (!was) osk.set(320);
      const vv = window.visualViewport!;
      fitted = el.getBoundingClientRect().bottom <= vv.offsetTop + vv.height + 0.5;
      if (!was) osk.set(0);
    }
    let acks = '';
    try { acks = sessionStorage.getItem('narrow-acks') ?? ''; } catch { acks = 'unreadable'; }
    return { keyboard, fitted, acks };
  }, overlay === 'none' ? '' : OVERLAY[overlay as keyof typeof OVERLAY]);
  return {
    pane: await page.locator('.app').getAttribute('data-pane'),
    view,
    selected,
    empty: (await page.locator('button.row').count()) === 0,
    welcome: (await page.locator('.welcome').count()) > 0,
    unseen: label.split(', ').includes('not seen yet'),
    overlay,
    keyboard: dom.keyboard,
    fitted: dom.fitted,
    backed: c.backed,
    acked_on_list: dom.acks.split(',').includes('list'),
  };
}

// Files the walk's session under the project, as another tab would, so
// the project page has it as a thread to open.
async function file(c: Ctx): Promise<void> {
  const res = await c.serve.api.post(`/api/sessions/${c.id}/project`, { data: { project: SLUG } });
  if (!res.ok()) throw new Error(`file under ${SLUG}: ${res.status()} ${await res.text()}`);
}

async function row(c: Ctx): Promise<{ status: string; unseen: boolean }> {
  const res = await c.serve.api.get('/api/sessions');
  const rows = (await res.json()).sessions as { id: string; status: string; unseen?: boolean }[];
  const r = rows.find((x) => x.id === c.id);
  return { status: r?.status ?? 'missing', unseen: Boolean(r?.unseen) };
}

async function until(c: Ctx, what: string, ok: () => Promise<boolean>): Promise<void> {
  for (let i = 0; i < 200; i++) {
    if (await ok()) return;
    await c.page.clock.fastForward(250);
    await new Promise((r) => setTimeout(r, 25));
  }
  throw new Error(`narrow_layout: ${what} never happened`);
}

const visible = (c: Ctx, sel: string) => c.page.locator(`${sel}:visible`).first();
const nextName = (c: Ctx, p: string) => `${p}${String(++c.turn).padStart(4, '0')}`;

// The first thing on the screen that takes typing, for KeyboardUp.
async function focusTyping(c: Ctx): Promise<void> {
  const { page } = c;
  if (await page.locator(OVERLAY.settings).isVisible()) {
    // The model picker's search field: the one text field Settings has.
    // The combobox toggles: a second KeyboardUp (after KeyboardDown) finds
    // the picker still open, and a click would close it.
    if (!(await page.locator('.head-pop .sel-search').isVisible())) {
      await page.locator('.head-pop').getByRole('combobox', { name: /^Next turn model/ }).click();
    }
    await page.locator('.head-pop .sel-search').focus();
  } else if (await page.locator(OVERLAY.panel).isVisible()) {
    await page.locator('.prj-panel textarea').first().focus();
  } else if (await page.locator('#composer').isVisible()) {
    await page.locator('#composer').focus();
  } else {
    await page.getByRole('textbox', { name: 'Message the project' }).focus();
  }
}

modelTests<Ctx>({
  spec: 'narrow_layout',
  role: 'Phone#0',
  // No `config` here: it would replace the serveOpts above (the key)
  // with a bare config; CONTROL_CONFIG is set there.

  async init(page, serve) {
    const c: Ctx = { page, serve, id: '', turn: 0, held: [], holdAcks: false, acks: [], backed: false };
    const res = await serve.api.post('/api/projects', { data: { name: 'Demo' } });
    if (!res.ok()) throw new Error(`create project: ${res.status()} ${await res.text()}`);
    await page.addInitScript(phone);
    // A key that "could not be checked" counts as working (welcome.tsx
    // keyLine), and no check leaves the machine.
    await page.route('**/api/setup/check**', (r) => r.fulfill({ status: 200, contentType: 'application/json', body: '{"state":"unknown"}' }));
    await page.route('**/api/sessions/*/ack', (r) => {
      if (c.holdAcks) c.acks.push(r);
      else void r.continue();
    });
    await page.goto(serve.url + '/');
    return c;
  },

  actions: {
    OpenRow: async (c) => { c.backed = false; await visible(c, `button.row[data-id="${c.id}"]`).click(); },

    // "/" from a hardware keyboard, which does nothing while focus is in
    // a text field; a person taps off it first.
    async SearchKey(c) {
      c.backed = false;
      await c.page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
      await c.page.keyboard.press('/');
    },

    Back: async (c) => { c.backed = true; await visible(c, 'button.back').click(); },
    NavPage: async (c) => { c.backed = false; await visible(c, '.phone-nav a[href="#/projects"]').click(); },
    NavSessions: async (c) => { c.backed = false; await visible(c, '.phone-nav a[href="#/"]').click(); },
    OpenProject: async (c) => { c.backed = false; await visible(c, `a[href="#/projects/${SLUG}"]`).click(); },
    async OpenThread(c) {
      c.backed = false;
      // A thread nobody has written in yet sits in "Empty", folded
      // behind its "Show all" line on the project's home.
      const more = c.page.locator('.prj-home .prj-more');
      if (await more.isVisible()) await more.click();
      await visible(c, '.prj-home .prj-thread:not(.prj-main-row)').click();
    },
    Reload: async (c) => { await c.page.reload(); },

    // A suggestion starts a session with a real turn; it finishes while
    // on screen, so the page acks it at once and nothing is left unseen.
    async WelcomeStart(c) {
      c.backed = false;
      const dir = controlDir(c.serve.home);
      const name = nextName(c, 't');
      queue(dir, name, { mode: 'ok', text: `finished ${name}` });
      // The chips wait for the folder check, which waits out a debounce
      // on the page's clock.
      const chip = c.page.getByRole('button', { name: 'Explain this repo' });
      await until(c, 'the welcome ready to start', () => chip.isEnabled());
      await chip.click();
      await waitTaken(dir, name);
      await until(c, 'the started session in the URL', async () => (await c.page.evaluate(() => location.hash)).startsWith('#/s/'));
      c.id = (await c.page.evaluate(() => location.hash)).slice('#/s/'.length);
      await until(c, 'the first turn done and seen', async () => { const r = await row(c); return r.status === 'done' && !r.unseen; });
      await file(c);
    },

    // Another client's New session: the page only sees it on the list.
    async Arrive(c) {
      c.id = await c.serve.newSession();
      await file(c);
    },

    OpenSettings: async (c) => { c.backed = false; await visible(c, 'button.more[aria-label="Session settings"]').click(); },

    // The Work button shows only while the session has work: a
    // background agent it spawned (through the API, as its own turn
    // would), held running until the walk ends.
    async OpenWork(c) {
      c.backed = false;
      const dir = controlDir(c.serve.home);
      const name = nextName(c, 'b');
      queue(dir, name, { mode: 'block', text: 'agent done' });
      const res = await c.serve.api.post('/api/sessions', { data: { cwd: c.serve.work, prompt: 'background work', spawnedBy: c.id } });
      if (!res.ok()) throw new Error(`spawn agent: ${res.status()} ${await res.text()}`);
      c.held.push(name);
      await waitTaken(dir, name);
      await until(c, 'the Work button', () => c.page.locator('.work-summary').isVisible());
      await c.page.locator('.work-summary').click();
    },

    OpenThreads: async (c) => { c.backed = false; await visible(c, '.prj-threads-btn').click(); },
    OpenPanel: async (c) => { c.backed = false; await visible(c, '.prj-panel-btn').click(); },

    async CloseOverlay(c) {
      const { page } = c;
      if (await page.locator(OVERLAY.settings).isVisible()) await visible(c, 'button.more[aria-label="Session settings"]').click();
      else if (await page.locator(OVERLAY.work).isVisible()) await page.locator('.work-sheet .work-close').click();
      else if (await page.locator(OVERLAY.threads).isVisible()) await page.locator('.prj-threads .prj-drawer-close').click();
      else await page.locator('.prj-panel .prj-drawer-close').click();
    },

    async KeyboardUp(c) {
      await focusTyping(c);
      await c.page.evaluate((px) => (window as unknown as { __osk: { set(px: number): void } }).__osk.set(px), KB);
    },
    KeyboardDown: async (c) => { await c.page.evaluate(() => (window as unknown as { __osk: { set(px: number): void } }).__osk.set(0)); },

    // A turn in the walk's session, sent by another client (the list has
    // no composer), run to the end while the list is the pane.
    async Finish(c) {
      const dir = controlDir(c.serve.home);
      const name = nextName(c, 't');
      queue(dir, name, { mode: 'block', text: `finished ${name}` });
      const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: `turn ${name}` } });
      if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
      await waitTaken(dir, name);
      c.holdAcks = true;
      release(dir, name);
      await until(c, `turn ${name} done`, async () => { const r = await row(c); return r.status === 'done' && r.unseen; });
    },

    // The page's own ack, let through: it must have sent one.
    async Ack(c) {
      await until(c, 'the page to ack the finish on screen', async () => c.acks.length > 0);
      c.holdAcks = false;
      for (const r of c.acks.splice(0)) await r.continue();
    },
  },

  read: readUiState,
  // Whatever pane is showing carries the state.
  status: (c) => c.page.locator('.app[data-pane="list"] .sidebar, .app[data-pane="thread"] .app-main'),
  sessions: (c) => (c.id ? [c.id] : []),
  async cleanup(c) {
    const dir = controlDir(c.serve.home);
    for (const name of c.held.splice(0)) release(dir, name);
  },
});

