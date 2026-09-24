// go/tests/model/specs/ui_dialogs.fizz walked in the browser (recipe:
// go/tests/model/README.md): the archive confirm, the new-session
// palette, a project's typed delete and the ? shortcut sheet, with where
// focus is at every node and where it goes back to. Every generated path
// is one test; at each node readUiState must equal the spec's Page#0
// state, and the dialogs owe the surface checks (helpers/ui-invariants.ts).
//
// The walk decides when a create answers: POST /api/sessions from the
// page is held at the network until the spec's Created, which is what
// makes "Starting a session…" a state the page sits in. Archive,
// unarchive and delete answer within their one request, as the spec has
// them.
//
// One serve per worker: Init starts a fresh session, puts the project
// back on disk and loads the page afresh; sessions from earlier paths
// stay in the sidebar, which nothing here reads but the open one's row.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import { expect, test, type Serve } from '../../helpers/serve';
import { uiInvariants } from '../../helpers/ui-invariants';

const SLUG = 'alpha';
const NAME = 'Alpha';
const WRONG = 'alpah';

// A transitions walk is up to ~80 nodes, each with an axe pass on
// whatever dialog is up: about 30 s here, more on a loaded runner.
test.describe.configure({ timeout: 180_000 });
// A click on something that never shows fails the step, not the walk's whole budget.
test.use({ actionTimeout: 15_000 });

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;        // ~/.bough/projects/alpha
  current: string;    // the session the route "session" shows
  creates: Route[];   // held POST /api/sessions
  opener: string;     // where the dialog now up sends focus back, as the spec names it
  typed: string;      // delete's input as last left, as the spec keeps it
}

// What has focus, in the spec's words.
async function focusNow(page: Page): Promise<string> {
  return page.evaluate(() => {
    const a = document.activeElement as HTMLElement | null;
    if (!a || a === document.body || a === document.documentElement) return 'body';
    if (a.closest('[role=dialog][aria-modal=true]')) {
      if (a.tagName === 'INPUT') return 'field';
      const acts = a.closest('.dlg-actions');
      if (acts) {
        const t = (a.textContent ?? '').trim();
        if (t === 'Cancel' || t === 'Close') return 'cancel';
        if (a.matches('.btn-primary, .btn-danger')) return 'ok';
      }
      return `other in dialog: ${a.outerHTML.slice(0, 80)}`;
    }
    if (a.matches('.more[aria-label="Session settings"]')) return 'settings';
    if (a.closest('.side-nav, .sidebar')) return 'page';
    return `other: ${a.outerHTML.slice(0, 80)}`;
  });
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const dom = await page.evaluate(({ slug, id }) => {
    const hash = window.location.hash;
    const route = hash.startsWith('#/s/') ? 'session' : hash.startsWith('#/projects') ? 'projects' : `other: ${hash}`;
    let dialog = 'none', typed = '', failed = false;
    const dlg = document.querySelector('.dlg[role=dialog]');
    if (dlg) {
      const title = (dlg.querySelector('.dlg-title')?.textContent ?? '').trim();
      dialog = title === 'Archive this session?' ? 'archive' : title === 'Keyboard shortcuts' ? 'keys'
        : title.startsWith('Delete') ? 'delete' : `other: ${title}`;
      const v = (dlg.querySelector('input') as HTMLInputElement | null)?.value.trim();
      if (v !== undefined) typed = v === '' ? 'empty' : v === slug ? 'slug' : 'wrong';
      failed = !!dlg.querySelector('.dlg-err[role=alert]');
    } else if (document.querySelector('.pal[role=dialog]')) dialog = 'new';
    // The open session's flag: its composer note on the session page;
    // from the Projects page, whether its sidebar row has left the
    // list for Archived.
    let archived: boolean | string;
    if (route === 'session') archived = !!document.querySelector('.archived-note');
    else {
      const r = document.querySelector(`.sidebar button.row[data-id="${id}"]`);
      archived = !r || !!r.closest('#sec-archived');
    }
    const project = route === 'projects'
      ? [...document.querySelectorAll('section.proj')].some((s) => s.querySelector(`a[href="#/projects/${slug}"]`))
      : null;
    const creating = [...document.querySelectorAll('.updated[role=status]')].some((e) => /Starting a session/.test(e.textContent ?? ''));
    return { route, dialog, typed, failed, archived, project, creating };
  }, { slug: SLUG, id: c.current });
  return {
    route: dom.route,
    dialog: dom.dialog,
    focus: await focusNow(page),
    // No page shows who opened a dialog until it closes (focus goes back
    // there, which `focus` checks); this is the walk's record of it.
    opener: dom.dialog === 'none' ? 'none' : c.opener,
    // The spec keeps what was typed after the dialog goes; the page does
    // not, so between dialogs it is the walk's record.
    typed: dom.dialog === 'delete' ? dom.typed : c.typed,
    failed: dom.failed,
    archived: dom.archived,
    // The Projects page lists it; elsewhere nothing on screen does, so
    // the definition on disk says (a project is its directory).
    project: dom.project ?? fs.existsSync(path.join(c.dir, 'project.yml')),
    creating: dom.creating,
  };
}

async function heldCreate(c: Ctx): Promise<Route> {
  const deadline = Date.now() + 10_000;
  while (c.creates.length === 0) {
    if (Date.now() > deadline) throw new Error('ui_dialogs: no create to answer');
    await new Promise((r) => setTimeout(r, 25));
  }
  return c.creates.shift()!;
}

async function retype(c: Ctx, text: string): Promise<void> {
  await c.page.keyboard.press('ControlOrMeta+a');
  await c.page.keyboard.type(text);
}

modelTests<Ctx>({
  spec: 'ui_dialogs',
  role: 'Page#0',
  shared: true,

  async init(page, serve) {
    const dir = path.join(serve.home, '.bough', 'projects', SLUG);
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, 'project.yml'), `name: ${NAME}\nrepos: []\n`);
    const current = await serve.newSession();
    const c: Ctx = { page, serve, dir, current, creates: [], opener: 'none', typed: 'empty' };
    await page.route((u) => u.pathname === '/api/sessions', (r) => {
      if (r.request().method() === 'POST') c.creates.push(r);
      else void r.continue();
    });
    await page.goto(`${serve.url}/#/s/${current}`);
    await expect(page.locator('.more[aria-label="Session settings"]')).toBeVisible();
    return c;
  },

  actions: {
    GoProjects: (c) => c.page.locator('.side-nav a[href="#/projects"]').first().click(),
    async GoSession(c) {
      const row = c.page.locator(`.sidebar button.row[data-id="${c.current}"]`);
      // An archived row is under the Archived fold, and an older one
      // under its folder's "N more": open what hides it, as a person would.
      for (let i = 0; i < 4 && !(await row.count()); i++) {
        const fold = c.page.locator('.sidebar .sec-fold', { hasText: 'Archived' });
        if ((await fold.getAttribute('aria-expanded')) !== 'true') await fold.click();
        else await c.page.locator('.sidebar button.ws-older[aria-expanded="false"]').first().click({ timeout: 5_000 });
        await row.or(c.page.locator('.sidebar button.ws-older[aria-expanded="false"]')).first().waitFor({ timeout: 5_000 });
      }
      await row.click({ timeout: 10_000 });
    },
    async ArchiveMenu(c) {
      await c.page.locator('.more[aria-label="Session settings"]').click();
      // closeMore(true) puts focus on Settings before askConfirm reads it.
      c.opener = 'settings';
      await c.page.locator('.head-pop-item', { hasText: 'Archive…' }).click();
    },
    Unarchive: (c) => c.page.locator('.archived-note').getByRole('button', { name: 'Unarchive' }).click(),
    async Shortcuts(c) {
      c.opener = await focusNow(c.page);
      await c.page.keyboard.press('?');
    },
    async NewSession(c) {
      c.opener = await focusNow(c.page);
      await c.page.keyboard.press('Alt+KeyN');
    },
    async DeleteMenu(c) {
      await c.page.locator(`section.proj:has(a[href="#/projects/${SLUG}"])`).getByRole('button', { name: /^More actions for / }).click();
      // The menu item is the opener DialogHost records, and it unmounts
      // as the menu closes: nothing to go back to.
      c.opener = 'body';
      c.typed = 'empty';
      await c.page.getByRole('menuitem', { name: 'Delete…', exact: true }).click();
    },
    async TypeWrong(c) { c.typed = 'wrong'; await retype(c, WRONG); },
    async TypeSlug(c) { c.typed = 'slug'; await retype(c, SLUG); },
    Tab: (c) => c.page.keyboard.press('Tab'),
    Dismiss: (c) => c.page.keyboard.press('Escape'),
    async Confirm(c) {
      if (await c.page.locator('.pal[role=dialog]').count()) {
        // A first message, then Enter on the palette's Start.
        await c.page.keyboard.type('hello there');
        await expect(c.page.locator('.pal')).toContainText('Start a session');
        await c.page.keyboard.press('Enter');
        return;
      }
      await c.page.keyboard.press('Enter');
    },
    async Created(c) {
      const r = await heldCreate(c);
      await r.continue();
      // openSession pushes the new session's route once the answer lands.
      const opened = () => /^#\/s\/([^/?]+)$/.exec(new URL(c.page.url()).hash)?.[1];
      await expect.poll(() => opened() !== undefined && opened() !== c.current, { message: 'Created: the new session opens' }).toBe(true);
      c.current = opened()!;
    },
  },

  read: readUiState,
  status: (c) => c.page.locator('.side-nav'),
  sessions: () => [],
  // ui-invariants, not surface.ts: its ring check knows a text field
  // shows focus with its caret, and the palette's field drops the ring
  // on purpose (4bd7ba86).
  async invariants(c, where) {
    // A dialog fades in: axe measuring contrast mid-fade reads text as
    // fainter than it is. Finite animations only (spinners loop).
    await c.page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => Number.isFinite(Number(a.effect?.getComputedTiming().endTime)))
      .map((a) => a.finished.catch(() => undefined))));
    await uiInvariants(c.page, {
      include: ['.dlg-scrim', '.pal'],
      text: '.dlg-title, .dlg-body, .dlg-err, .dlg-actions .btn, .keys-h, .updated-text',
    }, where);
  },
  async cleanup(c) {
    for (const r of c.creates.splice(0)) await r.continue().catch(() => undefined);
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
