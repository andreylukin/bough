// go/tests/model/specs/ui_projects.fizz walked in the browser (recipe:
// go/tests/model/README.md): the Projects page's empty state, its one
// project's ⋯ menu, and the New project / Rename / Delete dialogs, with
// another tab or an agent writing ~/.bough/projects/alpha between the
// person's steps. Every generated path is one test; at each node
// readUiState must equal the spec's Page#0 state, and the page owes the
// surface checks (helpers/surface.ts) on top of the shared invariants.
//
// The walk decides when the server answers a create or a rename: those
// requests are held at the network until the spec's Respond, which is
// what makes "Creating…" a state the page sits in. A delete is never
// held (the spec takes its answer as landing with the Submit).
//
// The page's clock is the walk's: the projects list is re-read by an
// action at once but by the poll only every 30 s, so a change made
// elsewhere shows exactly when ListLands moves the clock past that.
//
// One serve per worker: 263 short paths, and a boot per path would be
// most of the run. Init empties the slot on disk; each test has its own
// page, so nothing the last path held carries over.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';
import { surfaceChecks } from '../../helpers/surface';

const SLUG = 'alpha';

// Up to 17 nodes a path, each with an axe pass (about half a second):
// more than the suite's 30 s default leaves room for.
test.describe.configure({ timeout: 60_000 });

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;       // ~/.bough/projects/alpha
  held: Route[];     // create and rename requests the walk has not answered
  opener: string;    // what opened the dialog now up, as the spec names it
  renameFrom: string; // the name the rename dialog opened on
}

const dialog = (c: Ctx) => c.page.getByRole('dialog');
const section = (c: Ctx) => c.page.locator(`section.proj:has(a[href="#/projects/${SLUG}"])`);

// What the server has: the definition's name: line, or no directory.
function disk(c: Ctx): string {
  const yml = path.join(c.dir, 'project.yml');
  if (!fs.existsSync(yml)) return 'none';
  return /^name:\s*(.+)$/m.exec(fs.readFileSync(yml, 'utf8'))?.[1].trim() ?? 'unnamed';
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await c.page.evaluate((slug) => {
    const sec = [...document.querySelectorAll('section.proj')].find((s) => s.querySelector(`a[href="#/projects/${slug}"]`));
    const shown = sec ? (sec.querySelector('h2')?.textContent ?? '').trim() : 'none';
    const dlg = document.querySelector('[role=dialog]');
    let dialog = 'none', draft = 'blocked', saving = false, failed = false;
    if (dlg) {
      const title = (dlg.querySelector('.dlg-title')?.textContent ?? '').trim();
      dialog = title === 'New project' ? 'create' : title === 'Rename project' ? 'rename' : title.startsWith('Delete') ? 'delete' : `other: ${title}`;
      saving = dlg.getAttribute('aria-busy') === 'true';
      failed = !!dlg.querySelector('.dlg-err[role=alert]');
      const v = (dlg.querySelector('input') as HTMLInputElement | null)?.value.trim() ?? '';
      const primary = dlg.querySelector('.dlg-actions .btn:last-child') as HTMLButtonElement | null;
      // The primary is disabled while a save is out too: then the draft
      // is what went, classified by what it says.
      if (!saving && primary?.disabled) draft = 'blocked';
      else if (v === '') draft = 'blank but enabled';
      else if (dialog === 'create') draft = v === '!!!' ? 'bad' : 'good';
      else if (dialog === 'delete') draft = v === slug ? 'good' : 'bad';
      else draft = 'good';
    }
    const t = document.querySelector('.toast:not([data-leaving])');
    const toast = !!t && /^Couldn’t delete the project/.test(t.querySelector('.toast-title')?.textContent ?? '');
    const a = document.activeElement as HTMLElement | null;
    let focus = 'none';
    if (a && a !== document.body) {
      if (a.matches('[role=dialog] input')) focus = 'input';
      else if (a.matches('.proj-more')) focus = 'more';
      else if (a.tagName === 'BUTTON' && a.textContent?.trim() === 'New project…') focus = 'new';
      else focus = `other: ${a.outerHTML.slice(0, 80)}`;
    }
    return { shown, menu: !!document.querySelector('[role=menu]'), dialog, draft, saving, failed, toast, focus };
  }, SLUG);
  return {
    disk: disk(c),
    ...dom,
    // No page shows who opened a dialog until it closes (focus goes back
    // there, which `focus` checks); this is the walk's record of which
    // control it used, kept while the page shows a dialog.
    opener: dom.dialog === 'none' ? 'none' : c.opener,
  };
}

// The next create or rename the page sent, once it has.
async function next(c: Ctx): Promise<Route> {
  const deadline = Date.now() + 10_000;
  while (c.held.length === 0) {
    if (Date.now() > deadline) throw new Error('ui_projects: no create or rename to answer');
    await new Promise((r) => setTimeout(r, 25));
  }
  return c.held.shift()!;
}

async function type(c: Ctx, good: boolean): Promise<void> {
  const kind = (await dialog(c).locator('.dlg-title').innerText()).trim();
  let text: string;
  if (kind === 'New project') text = good ? 'Alpha' : '!!!';
  else if (kind === 'Rename project') text = c.renameFrom === 'Alpha' ? 'Beta' : 'Alpha';
  else text = good ? SLUG : 'alpah';
  // The box has focus and, opening, its text selected: typing replaces it.
  await c.page.keyboard.type(text);
}

async function menuItem(c: Ctx, name: string): Promise<void> {
  await c.page.getByRole('menuitem', { name, exact: true }).click();
  c.opener = 'item';
}

modelTests<Ctx>({
  spec: 'ui_projects',
  role: 'Page#0',
  shared: true,
  // Reads move the clock one short step (a dialog focuses its box on the
  // next frame, a dismissed toast leaves 130 ms later), never near the
  // 30 s the projects poll waits: only ListLands may re-read the list.
  pollStepMs: 50,
  // Chromium logs every refused request: the 400 for "!!!", the 409 for
  // a create that raced one, the 404 for a rename or delete that raced a
  // delete. Each is a state of the spec, shown in the dialog or a toast.
  allowConsole: /Failed to load resource: the server responded with a status of (400|404|409)/,

  async init(page, serve) {
    const dir = path.join(serve.home, '.bough', 'projects', SLUG);
    fs.rmSync(dir, { recursive: true, force: true });
    const c: Ctx = { page, serve, dir, held: [], opener: 'none', renameFrom: '' };
    await page.route((u) => u.pathname === '/api/projects' || u.pathname === `/api/projects/${SLUG}/rename`, (r) => {
      if (r.request().method() === 'POST') c.held.push(r);
      else void r.continue();
    });
    const listed = page.waitForResponse((r) => new URL(r.url()).pathname === '/api/projects');
    await page.goto(`${serve.url}/#/projects`);
    await listed;
    return c;
  },

  actions: {
    async NewProject(c) {
      const btn = c.page.getByRole('button', { name: 'New project…', exact: true });
      c.opener = (await btn.evaluate((el) => !!el.closest('.proj-empty-row'))) ? 'empty' : 'head';
      await btn.click();
    },
    OpenMenu: (c) => section(c).getByRole('button', { name: /^More actions for / }).click(),
    CloseMenu: (c) => c.page.keyboard.press('Escape'),
    async Rename(c) {
      c.renameFrom = (await section(c).locator('h2').innerText()).trim();
      await menuItem(c, 'Rename…');
    },
    Delete: (c) => menuItem(c, 'Delete…'),
    TypeGood: (c) => type(c, true),
    TypeBad: (c) => type(c, false),
    async Clear(c) {
      await c.page.keyboard.press('ControlOrMeta+a');
      await c.page.keyboard.press('Backspace');
    },
    Submit: (c) => c.page.keyboard.press('Enter'),
    Cancel: (c) => c.page.keyboard.press('Escape'),
    Respond: async (c) => (await next(c)).continue(),
    async DismissToast(c) {
      await c.page.locator('.toast').getByRole('button', { name: 'Dismiss' }).click();
    },
    // The poll's interval fires with the clock past the 30 s since the
    // last projects read.
    ListLands: (c) => c.page.clock.fastForward(31_000),
    // Under the name the list still shows, if it shows one (the spec's
    // ElsewhereCreate says why).
    async ElsewhereCreate(c) {
      const sec = section(c);
      const name = (await sec.count()) ? (await sec.locator('h2').innerText()).trim() : 'Alpha';
      fs.mkdirSync(c.dir, { recursive: true });
      fs.writeFileSync(path.join(c.dir, 'project.yml'), `name: ${name}\nrepos: []\n`);
    },
    async ElsewhereDelete(c) {
      fs.rmSync(c.dir, { recursive: true, force: true });
    },
  },

  read: readUiState,
  status: (c) => c.page.locator('.proj-page-head h1'),
  sessions: () => [],
  check: (c, where) => surfaceChecks(c.page, {
    include: ['.proj-page-head', '.proj-body', '.dlg-scrim', '.toast'],
    texts: '.proj-page-head h1, .proj-page-head .head-repo, section.proj h2, .proj-empty-row p, [role=menuitem], .dlg-title, .dlg-body, .dlg-err, .dlg-actions .btn, .toast-title, .toast-msg',
  }, where),
  async cleanup(c) {
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
