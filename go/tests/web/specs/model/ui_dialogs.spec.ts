// go/tests/model/specs/ui_dialogs.fizz in the browser: the archive
// confirm, the new-session palette, Delete project and the ? sheet, with
// the focus trap and where focus goes back to. Every action is a real
// click or key; the one server answer the spec steps on its own (a
// create's) is held at the network until the path says Created.
//
// One serve per worker: each walk gets its own session and its own
// project slug, and reads only those.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { uiInvariants } from '../../helpers/ui-invariants';
import { expect, test, type Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;       // the open session
  slug: string;     // the one project
  held: Route | null; // the create request, held until Created
  typed: string;    // delete's field as last drawn (see readUiState)
}

// A walk runs to ~45 steps (MODEL_COVER=transitions), each new node with
// an axe pass (1-3 s on a loaded runner).
test.describe.configure({ timeout: 600_000 });

let seq = 0;
const unique = (p: string) => `${p}${process.pid.toString(36)}${(++seq).toString(36)}${Date.now().toString(36).slice(-4)}`;

// Where focus goes back to is the page's own state (DialogHost's and the
// palette's opener refs), never drawn. This reads it off focus itself:
// the element that last had focus outside a modal when focus went into
// one, as long as it is still in the document; a removed one means focus
// will drop to <body>.
const OPENER_PROBE = () => {
  let prev: Element | null = null;
  let opener: Element | null = null;
  const inModal = (el: Element | null) => Boolean(el?.closest?.('[aria-modal="true"]'));
  document.addEventListener('focusin', (e) => {
    const t = e.target as Element;
    if (inModal(t) && !(prev?.isConnected && inModal(prev))) opener = prev?.isConnected ? prev : null;
    prev = t;
  }, true);
  // Focus let go to <body>, except by the page going inert under a
  // modal: that blur is the modal opening, and the opener stays.
  document.addEventListener('focusout', (e) => {
    if (!(e as FocusEvent).relatedTarget && !(e.target as Element).closest?.('[inert]')) prev = null;
  }, true);
  (window as unknown as { __opener: () => Element | null }).__opener = () => opener;
};

// What document.activeElement is, in the spec's words.
const FOCUS_WORDS = `(el) => {
  if (!el || el === document.body || el === document.documentElement) return 'body';
  if (el.closest('[aria-modal="true"]')) {
    if (el.matches('input, textarea')) return 'field';
    const t = (el.textContent || '').trim();
    return t === 'Cancel' || t === 'Close' ? 'cancel' : 'ok';
  }
  if (el.getAttribute('aria-label') === 'Session settings') return 'settings';
  if (el.matches('a[href], button')) return 'page';
  return 'other: ' + el.tagName.toLowerCase() + (el.id ? '#' + el.id : '') + (el.className ? '.' + String(el.className).split(' ').join('.') : '');
}`;

const modal = (p: Page) => p.locator('[role="dialog"][aria-modal="true"]');
const dlg = (p: Page) => p.locator('.dlg[role="dialog"]');
const pal = (p: Page) => p.locator('.pal[role="dialog"]');
const section = (c: Ctx) => c.page.locator(`section.proj:has(a[href="#/projects/${c.slug}"])`);

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const got = await p.evaluate((words) => {
    const cls = new Function('return ' + words)() as (el: Element | null) => string;
    const opener = (window as unknown as { __opener?: () => Element | null }).__opener?.() ?? null;
    return {
      hash: location.hash,
      focus: cls(document.activeElement),
      opener: opener?.isConnected ? cls(opener) : 'body',
    };
  }, FOCUS_WORDS);
  let dialog = 'none';
  // The field's text outlives the dialog (DialogHost clears it only when
  // the next one opens), and so does the spec's typed: with no delete
  // dialog up it is what the field last held.
  let typed = c.typed;
  let failed = false;
  const d = dlg(p);
  if (await pal(p).count()) {
    const ph = (await pal(p).getByRole('combobox').or(pal(p).locator('input')).first().getAttribute('placeholder')) ?? '';
    dialog = /first message/.test(ph) ? 'new' : `palette: ${ph}`;
  } else if (await d.count()) {
    const title = (await d.locator('.dlg-title').innerText()).trim();
    dialog = title === 'Archive this session?' ? 'archive' : title === 'Keyboard shortcuts' ? 'keys'
      : title.startsWith('Delete “') ? 'delete' : `other: ${title}`;
    if (dialog === 'delete') {
      const v = await d.locator('input').inputValue();
      typed = c.typed = v === '' ? 'empty' : v === c.slug ? 'slug' : 'wrong';
      failed = await d.locator('.dlg-err').isVisible();
    }
  }
  const route = got.hash.startsWith('#/s/') ? 'session' : got.hash.startsWith('#/projects') ? 'projects' : `other: ${got.hash}`;
  // The project is drawn only on the Projects page; on a session page it
  // is read off serve, as another client would.
  let project: boolean;
  if (route === 'projects') project = (await section(c).count()) > 0;
  else {
    const res = await c.serve.api.get('/api/projects');
    const { projects } = (await res.json()) as { projects: { slug: string }[] | null };
    project = (projects ?? []).some((x) => x.slug === c.slug);
  }
  return {
    route,
    dialog,
    focus: got.focus,
    opener: dialog === 'none' ? 'none' : got.opener,
    typed,
    failed,
    // The open session's flag: its composer note, or off serve while the
    // Projects page is up.
    archived: route === 'session' ? await p.locator('.archived-note').isVisible() : await archivedOnServe(c),
    project,
    creating: await p.getByRole('status').filter({ hasText: 'Starting a session…' }).isVisible(),
  };
}

async function archivedOnServe(c: Ctx): Promise<boolean> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  return ((await res.json()) as { session: { archived?: boolean } }).session.archived === true;
}

const scanned = new Set<string>();

// The invariants of this surface beyond the ones every flow owes
// (helpers/ui-invariants.ts): no clipped title, status line or refusal,
// a ring on whatever the keyboard is on, and axe clean over the dialog
// while one is up (the rest is inert), else the page the dialogs return
// to. The sidebar's list is ui_sidebar's surface, left to that walk.
async function invariants(c: Ctx, where: string): Promise<void> {
  const p = c.page;
  // axe takes 1-3 s a page, most of a step. The same abstract state with
  // focus on the same element draws the same surface, so each is scanned
  // once per worker: every node is still covered, repeats are not.
  const key = JSON.stringify([await readUiState(c), await p.evaluate(() => {
    const el = document.activeElement;
    return el ? `${el.tagName}.${el.className}:${(el.textContent ?? '').trim().slice(0, 40)}` : '';
  })]);
  const fresh = !scanned.has(key);
  if (fresh) {
    // CSS animations run on the real clock, not the page's: a dialog
    // still fading in reads as low contrast. Spinners loop and are left.
    await p.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => Number.isFinite(Number(a.effect?.getComputedTiming().endTime)))
      .map((a) => a.finished.catch(() => undefined))));
  }
  const surface = (await modal(p).count()) ? '[role="dialog"][aria-modal="true"]' : 'main.app-main';
  await uiInvariants(p, {
    // An empty include skips axe and keeps the text and ring checks.
    include: fresh ? [surface] : [],
    text: 'h1, h2, h3, .dlg-title, .dlg-body, .dlg-err, .keys-h, .updated-text, [role="status"]',
  }, where);
  scanned.add(key);
}

// Opens the project's ... menu and picks an item.
async function projectMenu(c: Ctx, item: string): Promise<void> {
  await section(c).getByRole('button', { name: /^More actions for / }).click();
  await c.page.getByRole('menuitem', { name: item, exact: true }).click();
}

modelTests<Ctx>({
  spec: 'ui_dialogs',
  role: 'Page#0',
  config: CONTROL_CONFIG,
  shared: true,
  // No polls in the spec; the clock only has to let rAF focus land.
  pollStepMs: 100,

  async init(page, serve) {
    const c: Ctx = { page, serve, id: '', slug: unique('p'), held: null, typed: 'empty' };
    const dir = path.join(serve.home, '.bough', 'projects', c.slug);
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, 'project.yml'), `name: ${c.slug}\nrepos: []\n`);
    c.id = await serve.newSession();
    await page.addInitScript(OPENER_PROBE);
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await expect(page.locator('h1')).toBeVisible();
    return c;
  },

  actions: {
    async GoProjects(c) {
      await c.page.locator('a.side-nav-item', { hasText: 'Projects' }).click();
      await expect(c.page.getByRole('heading', { level: 1, name: 'Projects' })).toBeVisible();
    },
    async GoSession(c) {
      // The session's sidebar row; an archived one is under Archived,
      // which is folded until opened.
      const row = c.page.locator(`button.row[data-id="${c.id}"]`).first();
      if (!(await row.isVisible())) {
        const fold = c.page.getByRole('treeitem', { name: /^Archived/ });
        if ((await fold.getAttribute('aria-expanded')) !== 'true') await fold.click();
        // A folder there shows its newest few, the rest behind "N more".
        const more = c.page.locator('#sec-archived').getByRole('treeitem', { name: /^\d+ more$/ });
        // Loading it runs on the page's timers, which only move when told.
        await expect.poll(async () => {
          await c.page.clock.fastForward(100);
          return row.or(more).first().isVisible();
        }, { message: 'the Archived section' }).toBe(true);
        if (!(await row.isVisible())) await more.first().click();
      }
      await row.click();
      await expect(c.page.locator('h1')).toBeVisible();
    },
    async ArchiveMenu(c) {
      await c.page.getByRole('button', { name: 'Session settings' }).click();
      await c.page.locator('.head-pop-item', { hasText: /^Archive…$/ }).click();
      await dlg(c.page).waitFor();
    },
    async Unarchive(c) {
      const done = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/unarchive`));
      await c.page.locator('.archived-note').getByRole('button', { name: 'Unarchive' }).click();
      await done;
    },
    Shortcuts: (c) => c.page.keyboard.press('?'),
    async NewSession(c) {
      await c.page.keyboard.press('Alt+KeyN');
      await pal(c.page).waitFor();
    },
    DeleteMenu: (c) => projectMenu(c, 'Delete…'),
    TypeWrong: (c) => dlg(c.page).locator('input').fill('not-it'),
    TypeSlug: (c) => dlg(c.page).locator('input').fill(c.slug),
    Tab: (c) => c.page.keyboard.press('Tab'),
    Dismiss: (c) => c.page.keyboard.press('Escape'),

    async Confirm(c) {
      const p = c.page;
      if (await pal(p).count()) {
        // The create is held here; Created lets it through.
        await p.route('**/api/sessions', async (route) => {
          if (route.request().method() !== 'POST') return route.fallback();
          c.held = route;
        });
        await pal(p).locator('input').first().fill('a first message');
        await pal(p).getByRole('option', { name: /^Start a session/ }).click();
        await expect.poll(() => c.held !== null, { message: 'the create request' }).toBe(true);
        return;
      }
      const title = (await dlg(p).locator('.dlg-title').innerText()).trim();
      if (title === 'Archive this session?') {
        const done = p.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/archive`));
        await p.keyboard.press('Enter');
        await done;
        return;
      }
      // Delete: Enter in the field or on the button submits alike.
      const typed = await dlg(p).locator('input').inputValue();
      const done = typed === c.slug ? p.waitForResponse((r) => r.url().endsWith(`/api/projects/${c.slug}`) && r.request().method() === 'DELETE') : null;
      await p.keyboard.press('Enter');
      if (done) await done;
    },

    async Created(c) {
      const route = c.held;
      if (!route) throw new Error('Created with no create held');
      c.held = null;
      queue(controlDir(c.serve.home), unique('t'), { mode: 'ok', text: 'started' });
      const answered = c.page.waitForResponse((r) => r.url().endsWith('/api/sessions') && r.request().method() === 'POST');
      await route.continue();
      await c.page.unroute('**/api/sessions');
      const res = await answered;
      c.id = ((await res.json()) as { session: { id: string } }).session.id;
      await expect(c.page).toHaveURL(new RegExp(`#/s/${c.id}$`));
    },
  },

  read: readUiState,
  invariants,
  // The route's own heading: the session's title, or Projects.
  status: (c) => c.page.locator('h1').first(),
  // This graph describes the page, so it has no history projection.
  sessions: () => [],
  async cleanup(c) {
    if (c.held) await c.held.abort().catch(() => {});
  },
});
