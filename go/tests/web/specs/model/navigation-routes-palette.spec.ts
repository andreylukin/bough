// go/tests/model/specs/navigation_routes_palette.fizz in the browser:
// one tab's routes, Back, the arrival pick, the palette and the shortcuts
// sheet, walked along every generated path against a real serve. The
// recipe is go/tests/model/README.md; example.spec.ts is the worked one.
//
// The fixture is the spec's: S, the one listed session, with an unseen
// finish; X, a session the list does not hold (archived); a project slug
// nothing has. Serve is per test (modelTests' fixture, not the worker's):
// the arrival pick reads the whole list, so another test's sessions in
// it would change what arrival opens.
//
// Two things the spec treats as the system's choice are steered here by
// holding the page's own requests (page.route), never by faking the
// server's answer:
//   - the first list read is held from mount, so Init is "arrival not
//     decided yet"; ArriveLate lets it through after the page's 400 ms.
//   - X's lookup (GET /api/sessions/X) is held, so "Loading session…" is
//     a state; Found lets it through, NotFound and LoadFailed send it on
//     to an id the server never had (its 404) and to a session whose
//     transcript is unreadable (its 500).
import type { Page, Route } from '@playwright/test';
import * as fs from 'fs';
import * as path from 'path';
import { CONTROL_CONFIG, controlDir, queue } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { expect, test, type Serve } from '../../helpers/serve';

const GONE_SLUG = 'no-such-project';
// A project label id from before projects were keyed by slug: the page
// redirects it to the project list.
const OLD_PROJECT = '0190a8c2-0000-7000-8000-000000000000';
// An id this server never had: its lookup is an authoritative 404.
const GONE_ID = '01999999-0000-7000-8000-000000000000';
const S_TITLE = 'Session S';
const X_TITLE = 'Lookup X';

interface Ctx {
  page: Page;
  serve: Serve;
  s: string;       // S: listed, unseen finish
  x: string;       // X: archived, so the list does not hold it
  broken: string;  // archived, transcript unreadable: its read is a 500
  listHeld: boolean;
  heldList: Route[];
  heldLookup: Route[];
  // The history index from which Back is modelled: the spec models one
  // Back and no further, so entries before a Back (or before Init) read
  // as "none".
  floor: number;
}

async function waitDone(serve: Serve, id: string): Promise<void> {
  await expect.poll(async () => {
    const r = await serve.api.get(`/api/sessions/${id}`);
    const row = r.ok() ? (await r.json()).session : {};
    return row.status === 'done' && row.unseen === true;
  }, { message: `session ${id} never finished`, timeout: 40_000 }).toBe(true);
}

async function finished(serve: Serve, name: string, title: string): Promise<string> {
  queue(controlDir(serve.home), name, { mode: 'ok', text: `finished ${name}` });
  const id = await serve.newSession(`turn ${name}`);
  await waitDone(serve, id);
  const r = await serve.api.post(`/api/sessions/${id}/rename`, { data: { title } });
  if (!r.ok()) throw new Error(`rename: ${r.status()} ${await r.text()}`);
  return id;
}

async function archive(serve: Serve, id: string): Promise<void> {
  const r = await serve.api.post(`/api/sessions/${id}/archive`);
  if (!r.ok()) throw new Error(`archive: ${r.status()} ${await r.text()}`);
}

// The route a hash names, in the spec's words.
function hashName(c: Ctx, h: string): string {
  const r = h.replace(/^#\/?/, '');
  if (r === '') return 'home';
  if (r === `s/${c.s}`) return 's';
  if (r === `s/${c.s}/context`) return 'sub';
  if (r === `s/${c.x}`) return 'other';
  if (r === 'projects') return 'projects';
  if (r === `projects/${GONE_SLUG}`) return 'project_gone';
  if (r === `projects/${OLD_PROJECT}`) return 'old_project';
  if (r === 'nothing-here') return 'unknown';
  return `raw:${h}`;
}

const main = (c: Ctx) => c.page.locator('main.app-main');
const visible = async (l: ReturnType<Page['locator']>) => (await l.count()) > 0 && l.first().isVisible();

// What the main pane shows, by the words and headings a person sees.
async function readPage(c: Ctx): Promise<string> {
  const m = main(c);
  if (await visible(m.getByText('Nothing at this address', { exact: true }))) return 'lost';
  if (await visible(m.getByText('Session not found', { exact: true }))) return 'missing';
  if (await visible(m.getByText('Couldn’t load this session', { exact: true }))) return 'loadfail';
  if (await visible(m.locator('.lookup', { hasText: 'Loading session…' }))) return 'loading';
  if (await visible(m.getByText('Project not found', { exact: true }))) return 'project_missing';
  if (await visible(m.getByText('Couldn’t read this project', { exact: true }))) return 'project_error';
  if (await visible(m.locator('.ov'))) return 'overview';
  if (await visible(m.locator('.page-head h1', { hasText: /^Context$/ }))) return 'sub';
  if (await visible(m.locator('h1', { hasText: /^Projects$/ }))) return 'projects';
  const head = m.locator('.thread-head:not(.page-head) h1');
  if (await visible(head)) {
    const t = (await head.first().textContent()) ?? '';
    if (t === S_TITLE) return 'session';
    if (t === X_TITLE) return 'other_session';
    return `thread:${t}`;
  }
  return 'blank';
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const nav = await p.evaluate(() => {
    const n = (window as unknown as { navigation: { currentEntry: { index: number }; entries(): { url: string }[] } }).navigation;
    const i = n.currentEntry.index;
    return { hash: location.hash, i, prev: i > 0 ? new URL(n.entries()[i - 1].url).hash : null };
  });
  const pal = p.locator('.pal[role="dialog"]');
  const sheet = p.getByRole('dialog', { name: 'Keyboard shortcuts' });
  const overlay = (await visible(pal)) ? 'pal' : (await visible(sheet)) ? 'sheet' : 'none';
  let sel = '';
  if (overlay === 'pal') {
    const on = pal.locator('.pal-item.pal-on');
    sel = (await on.count()) === 0 ? 'none'
      : (await on.first().getAttribute('class'))?.includes('pal-bad-item') ? 'chosen' : 'safe';
  }
  // S's sidebar row: the list has landed (the arrival is decided) once
  // it is there, and its label says whether its finish is unseen.
  const row = p.locator(`button.row[data-id="${c.s}"]`).first();
  const listed = (await row.count()) > 0;
  const label = listed ? (await row.getAttribute('aria-label')) ?? '' : '';
  return {
    hash: hashName(c, nav.hash),
    page: await readPage(c),
    back: nav.i > c.floor && nav.prev !== null ? hashName(c, nav.prev) : 'none',
    overlay,
    sel,
    // Before the list lands there is no row to read, and nothing the page
    // could have acked (the ack needs S's row): the fixture's unseen stands.
    unseen: listed ? label.split(', ').includes('not seen yet') : true,
    arrived: listed,
  };
}

// The first list read lands now: the page is past its 400 ms, so it is
// the late answer, which leaves whatever is on screen alone.
async function releaseList(c: Ctx): Promise<void> {
  if (!c.listHeld) return;
  c.listHeld = false;
  for (const r of c.heldList.splice(0)) await r.continue().catch(() => {});
  await expect(c.page.locator(`button.row[data-id="${c.s}"]`).first()).toBeVisible();
}

// Answers X's held lookup (the newest; older ones belong to a page that
// is gone) with what the server says for `id`.
async function settle(c: Ctx, id: string): Promise<void> {
  await expect.poll(() => c.heldLookup.length, { message: 'X lookup never asked' }).toBeGreaterThan(0);
  const url = `${c.serve.url}/api/sessions/${id}`;
  for (const r of c.heldLookup.splice(0)) await r.continue(id === c.x ? undefined : { url }).catch(() => {});
}

// A link typed or followed: a same-document navigation, a new entry.
async function follow(c: Ctx, route: string): Promise<void> {
  await releaseList(c);
  await c.page.evaluate((h) => { location.hash = h; }, route);
}

// Keys go to the page, not a field: ? and Esc are ignored while typing.
async function blur(c: Ctx): Promise<void> {
  await c.page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur?.());
}

async function palette(c: Ctx): Promise<ReturnType<Page['locator']>> {
  await blur(c);
  await c.page.keyboard.press('ControlOrMeta+k');
  const pal = c.page.locator('.pal[role="dialog"]');
  await expect(pal).toBeVisible();
  return pal;
}

async function palPick(c: Ctx, query: string, name: RegExp): Promise<void> {
  const pal = c.page.locator('.pal[role="dialog"]');
  const opt = pal.getByRole('option', { name }).first();
  // A reopened palette shows its last query for a moment before it clears
  // it, and the clear can land after the fill: the option then vanishes
  // under the click, which waited for it until the test timed out. So
  // the fill and the click are retried together, the click briefly.
  await expect(async () => {
    await pal.getByRole('combobox').fill(query);
    await opt.click({ timeout: 2000 });
  }).toPass({ timeout: 10_000 });
  await expect(pal).toBeHidden();
}

modelTests<Ctx>({
  spec: 'navigation_routes_palette',
  role: 'Nav#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    // Three finished turns before the walk, then up to seven steps. The
    // turns run side by side: one after another, a loaded machine took
    // longer than the budget. Which session takes which queued turn does
    // not matter; only S stays listed, so recency never picks between them.
    test.info().setTimeout(90_000);
    const [x, broken, s] = await Promise.all([
      finished(serve, 'a0001', X_TITLE), finished(serve, 'a0002', 'Broken'), finished(serve, 'a0003', S_TITLE),
    ]);
    await archive(serve, x);
    await archive(serve, broken);
    // chmod moves neither size nor mtime, so serve still knows the
    // session while reading its transcript fails.
    fs.chmodSync(path.join(serve.home, '.bough', 'history', broken + '.jsonl'), 0);
    const c: Ctx = { page, serve, s, x, broken, listHeld: true, heldList: [], heldLookup: [], floor: 0 };
    await page.route((u) => u.pathname === '/api/sessions' && !u.search, (r) => {
      if (c.listHeld && r.request().method() === 'GET') c.heldList.push(r);
      else void r.continue();
    });
    await page.route((u) => u.pathname === `/api/sessions/${x}` && !u.searchParams.has('since'), (r) => {
      c.heldLookup.push(r);
    });
    await page.goto(`${serve.url}/`);
    c.floor = await page.evaluate(() => (window as unknown as { navigation: { currentEntry: { index: number } } }).navigation.currentEntry.index);
    return c;
  },

  actions: {
    // The list answering within 400 ms of mount is the mount this tab
    // would have had with a fast server: the same #/ read again, the list
    // let through at once. A reload adds no history entry.
    async ArriveFast(c) {
      c.listHeld = false;
      const dead = c.heldList.splice(0);
      await c.page.reload();
      // The old document's read goes nowhere now; aborting it would log.
      for (const r of dead) await r.continue().catch(() => {});
      // Before the harness moves the page's clock: a read that jumped it
      // 5 s while the list was still on the wire would make it late.
      await expect(c.page.locator(`button.row[data-id="${c.s}"]`).first()).toBeVisible();
    },
    ArriveLate: releaseList,
    Found: (c) => settle(c, c.x),
    NotFound: (c) => settle(c, GONE_ID),
    LoadFailed: (c) => settle(c, c.broken),

    async ClickRow(c) {
      await releaseList(c);
      await c.page.locator(`button.row[data-id="${c.s}"]`).first().click();
    },
    // Home is goList: "Show all sessions" where the page offers it, else
    // the palette's Sessions (the desktop has no Back button).
    async GoHome(c) {
      await releaseList(c);
      const all = main(c).getByRole('button', { name: 'Show all sessions' });
      if (await visible(all)) { await all.first().click(); return; }
      await palette(c);
      await palPick(c, 'Sessions', /^Sessions$/);
    },
    async NavProjects(c) {
      await c.page.getByRole('navigation', { name: 'Views' }).getByRole('link', { name: 'Projects' }).click();
      await releaseList(c);
    },
    OpenOther: (c) => follow(c, `#/s/${c.x}`),
    OpenUnknown: (c) => follow(c, '#/nothing-here'),
    OpenGoneProject: (c) => follow(c, `#/projects/${GONE_SLUG}`),
    OpenSubLink: (c) => follow(c, `#/s/${c.s}/context`),
    OpenOldProject: (c) => follow(c, `#/projects/${OLD_PROJECT}`),

    // The runtime strip's context meter opens the Context page.
    OpenContext: (c) => main(c).locator('button[aria-label^="Context:"]').first().click(),
    async CloseSub(c) {
      await blur(c);
      await c.page.keyboard.press('Escape');
    },
    Retry: (c) => main(c).getByRole('button', { name: 'Retry' }).first().click(),

    async Back(c) {
      await c.page.goBack();
      c.floor = await c.page.evaluate(() => (window as unknown as { navigation: { currentEntry: { index: number } } }).navigation.currentEntry.index);
    },
    Reload: async (c) => { await c.page.reload(); },

    OpenPalette: async (c) => { await palette(c); },
    // Only the destructive command answers this.
    TypeArchive: (c) => c.page.locator('.pal[role="dialog"]').getByRole('combobox').fill('>archive this'),
    ArrowToArchive: (c) => c.page.locator('.pal[role="dialog"]').getByRole('combobox').press('ArrowDown'),
    PalClose: (c) => c.page.locator('.pal[role="dialog"]').getByRole('combobox').press('Escape'),
    // A session option is named "<status> <title> <age>".
    PalSession: (c) => palPick(c, S_TITLE, new RegExp(`\\b${S_TITLE}\\b`)),
    PalProjects: (c) => palPick(c, 'Projects', /^Projects$/),
    async PalShortcuts(c) {
      await palPick(c, 'Keyboard shortcuts', /^Keyboard shortcuts/);
    },
    async ShowShortcuts(c) {
      await blur(c);
      await c.page.keyboard.press('?');
    },
    SheetClose: (c) => c.page.keyboard.press('Escape'),
  },

  // The lookups the spec sends to a 404 (a gone session, a gone project)
  // and a 500 (the unreadable transcript); the page says so on screen,
  // which the state read checks.
  expectedErrors: [/^Failed to load resource: the server responded with a status of (404|500) /],
  read: readUiState,
  status: main,
  sessions: (c) => [c.s],
  async cleanup(c) {
    // So serve's teardown can read what it removes; gone already if it ran.
    try { fs.chmodSync(path.join(c.serve.home, '.bough', 'history', c.broken + '.jsonl'), 0o644); } catch { /* removed */ }
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
