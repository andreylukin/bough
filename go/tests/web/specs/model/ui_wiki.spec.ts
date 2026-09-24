// go/tests/model/specs/ui_wiki.fizz walked in the browser (recipe:
// go/tests/model/README.md): the wiki's index, a page with its cited-entry
// pane, editor, history and phone menu, one review flag, the activity
// note and the palette the wiki searches through. Every generated path is
// one test against a real serve over a seeded ~/.bough/wiki; at each node
// readUiState must equal the spec's Wiki#0 state, and the screen must
// pass uiInvariants (clipped text, focus ring, axe) besides the shared
// checks.
//
// The page decides nothing about when its reads answer, so the walk does:
// the first read of each screen (and the cited entry, a Save, a review
// decision) is held at the network until the spec's Loaded / LoadFail /
// PageMissing (SourceLoaded / SourceFail, SaveOk / SaveFail, ActOk /
// ActFail) lets it go. Held reads go on to the real serve; a failure is
// answered by the walk. Reads after a screen has loaded (polls, the
// reload after a save or a review decision) go straight through, as the
// spec says they are not steps.
//
// No model turn is part of this flow: the one server event, a flag
// landing, is an ingest's write, done here as the file write it is.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';
import { uiInvariants } from '../../helpers/ui-invariants';

// ——— The seeded wiki ——————————————————————————————————————————————————
//
// Ports carries everything a page can: a cited claim (its chip opens the
// pane), the one uncited claim the spec's `flag` is about, a superseded
// claim (Review's "Open the newer entry"), and a link to Gate. s1 is the
// cited session, s2 a finished session no ingest has read (Activity's
// "Started" note needs one waiting), r1 the ingest that wrote Gate
// (Activity's "+ new" link).
const UNCITED = '- The artifact port is 7683.';
const PORTS = [
  '# Ports', '', 'Who binds first wins.', '', '## Facts', '',
  '- The web port is 7683 `s1#4`.',
  UNCITED,
  '- **Outdated** (superseded by `s1#5`): the old port was 7682 `s1#4`.',
  '', '## See also', '- [Gate](gate.md)', '',
].join('\n');
const GATE = '# Gate\n\nRead the gate `s1#4`.\n\n## See also\n- [Ports](ports.md)\n';
const INDEX = '# Wiki index\n\n## bough\n\n- [Ports](topics/bough/ports.md) — who binds first\n- [Gate](topics/bough/gate.md) — the test gate\n';
const LOG = '# Log\n\n## [2026-09-10] ingest | s1#5 | New | topics/bough/gate.md\n';

function session(file: string, cwd: string, entries: [string, Record<string, unknown>][]): void {
  const at = Date.parse('2026-09-10T12:00:00Z');
  const lines = [['meta', { cwd }] as [string, Record<string, unknown>], ...entries].map(([kind, data], i) =>
    JSON.stringify({ seq: i + 1, at: new Date(at + i * 1000).toISOString(), kind, data }));
  fs.writeFileSync(file, lines.join('\n') + '\n');
  // Quiet for a day: a session touched in the last half hour is not pending.
  const old = new Date(Date.now() - 86_400_000);
  fs.utimesSync(file, old, old);
}

/** The wiki and history every path starts from, written over whatever the last path left. */
function seed(home: string): void {
  const wiki = path.join(home, '.bough', 'wiki');
  const hist = path.join(home, '.bough', 'history');
  fs.mkdirSync(path.join(wiki, 'topics', 'bough'), { recursive: true });
  fs.mkdirSync(hist, { recursive: true });
  fs.writeFileSync(path.join(wiki, 'index.md'), INDEX);
  fs.writeFileSync(path.join(wiki, 'log.md'), LOG);
  fs.writeFileSync(path.join(wiki, 'topics', 'bough', 'ports.md'), PORTS);
  fs.writeFileSync(path.join(wiki, 'topics', 'bough', 'gate.md'), GATE);
  session(path.join(hist, 's1.jsonl'), '/repo', [
    ['input', { text: 'which port' }], ['thinking', { text: 'hmm' }],
    ['result', { text: 'listening on 7683' }], ['assistant', { text: 'The web port is 7683.' }],
  ]);
  session(path.join(hist, 's2.jsonl'), '/repo', [['input', { text: 'not ingested yet' }], ['assistant', { text: 'ok' }]]);
  session(path.join(hist, 'r1.jsonl'), wiki, [
    ['input', { text: '/llm-wiki ingest s1' }], ['done', { files: ['topics/bough/gate.md', 'log.md'] }],
  ]);
}

const portsFile = (c: Ctx) => path.join(c.serve.home, '.bough', 'wiki', 'topics', 'bough', 'ports.md');

// ——— Holding the reads ————————————————————————————————————————————————

type Kind = 'index' | 'review' | 'activity' | 'page' | 'source' | 'save' | 'claim';

interface Ctx {
  page: Page;
  serve: Serve;
  /** Kinds whose requests are held until the walk answers them. */
  hold: Set<Kind>;
  held: Map<Kind, Route[]>;
  /** How POST /api/wiki/ingest answers the next click. */
  ingest: 'ok' | 'fail';
  /** The page 404 PageMissing asked for: Chromium logs it as an error. */
  missing: boolean;
  /** The activity note's last word, for the screens that do not show it. */
  note: string;
  /** Whether the last Save failed, for a page still loading (its alert
   *  renders only over a loaded page). */
  alert: boolean;
  escapeUnderDialog: boolean;
}

const KINDS: [RegExp, Kind][] = [
  [/^GET \/api\/wiki$/, 'index'],
  [/^GET \/api\/wiki\/review$/, 'review'],
  [/^GET \/api\/wiki\/activity$/, 'activity'],
  [/^GET \/api\/wiki\/page$/, 'page'],
  [/^GET \/api\/wiki\/source$/, 'source'],
  [/^PUT \/api\/wiki\/page$/, 'save'],
  [/^POST \/api\/wiki\/claim$/, 'claim'],
];

async function onWiki(c: Ctx, route: Route): Promise<void> {
  const req = route.request();
  const key = `${req.method()} ${new URL(req.url()).pathname}`;
  if (key === 'POST /api/wiki/ingest') {
    // A real ingest spawns `bough wiki run`; the note is about the answer.
    await (c.ingest === 'ok' ? route.fulfill({ json: { ok: true } }) : fail(route));
    return;
  }
  const kind = KINDS.find(([re]) => re.test(key))?.[1];
  if (!kind || !c.hold.has(kind)) {
    await route.continue();
    return;
  }
  c.held.get(kind)!.push(route);
}

// A failed request the page cannot tell from any other. A 500 would do the
// same, but Chromium logs every non-2xx fetch as a console error, which
// the per-node invariant forbids; a 200 whose body is not JSON fails the
// same req() call without that noise.
const fail = (r: Route) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json: read failed' });

/** The held requests of a kind, waiting for the first to arrive. */
async function heldOf(c: Ctx, kind: Kind): Promise<Route[]> {
  const deadline = Date.now() + 10_000;
  const q = c.held.get(kind)!;
  while (q.length === 0) {
    if (Date.now() > deadline) throw new Error(`ui_wiki: no ${kind} request to answer`);
    await new Promise((r) => setTimeout(r, 20));
  }
  return q.splice(0);
}

/** Lets every held request of a kind through and stops holding it. */
async function release(c: Ctx, kind: Kind): Promise<void> {
  const rs = await heldOf(c, kind);
  c.hold.delete(kind);
  for (const r of rs) await r.continue();
}

/** Fails every held request of a kind; the next one is held again. */
async function failHeld(c: Ctx, kind: Kind): Promise<void> {
  for (const r of await heldOf(c, kind)) await fail(r);
}

// A move to another screen: its read (and the cited entry's) is held.
// Whatever the screen left held stays unanswered, as a slow server's
// would: answering a read the page has moved on from lands its data in
// the new screen's slot (useLoad keeps no generation), which is not a
// state of this spec.
function moving(c: Ctx, to: Kind, cite = false): void {
  for (const k of ['review', 'activity', 'page', 'source'] as Kind[]) c.held.set(k, []);
  c.hold.add(to);
  if (cite) c.hold.add('source');
}

// ——— readUiState ——————————————————————————————————————————————————————

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await c.page.evaluate(() => {
    const $ = (s: string, root: ParentNode = document) => root.querySelector<HTMLElement>(s);
    const main = $('main.app-main')!;
    const hash = decodeURIComponent(location.hash);
    const route = hash === '#/wiki' ? 'index' : hash === '#/wiki/review' ? 'review'
      : hash === '#/wiki/activity' ? 'activity' : hash.startsWith('#/wiki/p/') ? 'page' : `other: ${hash}`;
    const cite = route === 'page' && hash.includes('~');
    const pane = $('.wk-src', main);
    const failedIn = (root: ParentNode) => Array.from(root.querySelectorAll('.error-note-title'))
      .some((e) => /^Couldn’t load/.test(e.textContent ?? ''));
    // The screen's own error, not the pane's.
    const screenFailed = Array.from(main.querySelectorAll('.error-note-title'))
      .some((e) => !e.closest('.wk-src') && /^Couldn’t load/.test(e.textContent ?? ''));
    let data = 'loading';
    if (Array.from(main.querySelectorAll('h2, .state-title, .empty-state h2')).some((e) => e.textContent === 'This page doesn’t exist')) data = 'missing';
    else if (screenFailed) data = 'error';
    else if (route === 'index' ? $('.wk-health', main)
      : route === 'review' ? Array.from(main.querySelectorAll('.proj-head h2')).find((e) => e.textContent === 'Not compiled')
      : route === 'activity' ? $('.wk-stats', main)
      : $('.wk-doc .wk-titleblock, .wk-doc #wk-body', main)) data = 'loaded';

    let source = 'none';
    if (cite) source = !pane ? 'loading' : $('.wk-ent', pane) ? 'loaded' : failedIn(pane) ? 'error' : 'loading';

    const editor = $('#wk-body', main);
    const save = editor && Array.from(main.querySelectorAll<HTMLButtonElement>('.hk-panel button')).find((b) => b.textContent === 'Save');
    const menuBtn = $('button[aria-label="Page actions"]', main);

    let item = 'none';
    const row = route === 'review' && Array.from(main.querySelectorAll('.wk-item'))
      .find((r) => r.querySelector('.wk-title-line')?.textContent?.startsWith('Uncited'));
    if (row) {
      const mark = Array.from(row.querySelectorAll<HTMLButtonElement>('button')).find((b) => b.textContent === 'Mark as inference');
      // "failed" is the error with both buttons back: the spec lets Act
      // try again from it.
      const said = row.textContent?.includes('Did not save —');
      item = said ? (mark?.disabled ? 'failed, buttons disabled' : 'failed') : mark?.disabled ? 'busy' : 'open';
    }

    // Where the page shows whether the claim is still flagged: the index
    // counts it with the superseded one, Review lists its row.
    let flagShown: boolean | null = null;
    if (data === 'loaded' && route === 'index') {
      const link = $('.wk-link-bad', main)?.textContent ?? '';
      flagShown = link === 'Review 2 flagged claims' ? true : link === 'Review 1 flagged claim' ? false : null;
    } else if (data === 'loaded' && route === 'review') flagShown = Boolean(row);

    const note = route === 'activity' && data === 'loaded' ? ($('.page-head .hk-note[role=status]', main)?.textContent ?? '') : null;
    return {
      viewport: innerWidth <= 720 ? 'narrow' : 'wide',
      route, data, cite, source,
      panel: editor ? 'edit' : $('.wk-history', main) ? 'history' : 'none',
      saving: Boolean(save?.disabled),
      alert: route === 'page' && data === 'loaded' ? Boolean($('.wk-doc > p.hk2-alert', main)) : null,
      menu: menuBtn?.getAttribute('aria-expanded') === 'true',
      item,
      note,
      palette: Boolean($('.pal')),
      flagShown,
    };
  });
  // The claim on disk is what a review decision and an ingest change; the
  // page shows it only on the index and Review, where the two must agree.
  const flag = fs.readFileSync(portsFile(c), 'utf8').includes('\n' + UNCITED + '\n');
  const { flagShown, note, alert, ...rest } = dom;
  // WikiPageView keeps a failed save's error from page to page but shows
  // it only once the page has loaded: until then it is what it last was.
  if (alert !== null) c.alert = alert;
  else if (dom.route !== 'page') c.alert = false;
  // The note shows only on a loaded Activity; elsewhere it is what the
  // last one said (the spec keeps it in WikiPage across screens).
  let word = c.note;
  if (note !== null) word = note === '' ? '' : note.startsWith('Started') ? 'started' : 'failed';
  return {
    ...rest,
    flag: flagShown === null || flagShown === flag ? flag : `page says ${flagShown}, disk says ${flag}`,
    note: word,
    alert: c.alert,
    escape_under_dialog: c.escapeUnderDialog,
  };
}

// ——— Actions ——————————————————————————————————————————————————————————

const main = (c: Ctx) => c.page.locator('main.app-main');
const head = (c: Ctx) => main(c).locator('.page-head');
const ports = (c: Ctx) => c.page.locator('.wk-item').filter({ has: c.page.locator('.wk-title-line', { hasText: /^Uncited/ }) });
const hashNow = (c: Ctx) => c.page.evaluate(() => decodeURIComponent(location.hash));
const narrow = async (c: Ctx) => (await c.page.evaluate(() => innerWidth)) <= 720;
const WIDE = { width: 1100, height: 700 };
const NARROW = { width: 400, height: 800 };

async function routeKind(c: Ctx): Promise<Kind> {
  const h = await hashNow(c);
  return h === '#/wiki' ? 'index' : h === '#/wiki/review' ? 'review' : h === '#/wiki/activity' ? 'activity' : 'page';
}

async function openHit(c: Ctx): Promise<void> {
  const input = c.page.locator('.pal input');
  if ((await input.inputValue()).trim() === '') await input.fill('ports');
  // The palette asks the wiki 180 ms (page time) after the query settles.
  // A hit for another page than the one on screen: picking the page that
  // is already up changes no hash, so nothing reloads (not a move).
  const here = (await hashNow(c)).replace(/^#\/wiki\/p\/|~.*$/g, '');
  const hit = c.page.locator(`.pal [id^="pal-w:"]:not([id="pal-w:${here}"])`).first();
  const deadline = Date.now() + 10_000;
  while (!(await hit.count())) {
    if (Date.now() > deadline) throw new Error('ui_wiki: no wiki hit in the palette');
    await c.page.clock.fastForward(250);
    await new Promise((r) => setTimeout(r, 25));
  }
  moving(c, 'page');
  await hit.click();
}

// axe on every new screen is most of a walk's time; the longest path
// has eleven nodes.
test.describe.configure({ timeout: 60_000 });

modelTests<Ctx>({
  spec: 'ui_wiki',
  role: 'Wiki#0',
  shared: true,

  async init(page, serve) {
    seed(serve.home);
    const c: Ctx = {
      page, serve, hold: new Set(['index']), held: new Map(), ingest: 'ok', missing: false, note: '', alert: false, escapeUnderDialog: false,
    };
    for (const [, k] of KINDS) c.held.set(k, []);
    await page.setViewportSize(WIDE);
    await page.route('**/api/wiki**', (r) => onWiki(c, r));
    await page.goto(`${serve.url}/#/wiki`);
    await head(c).waitFor();
    return c;
  },

  actions: {
    async Loaded(c) { await release(c, await routeKind(c)); },
    async LoadFail(c) { await failHeld(c, await routeKind(c)); },
    async PageMissing(c) {
      c.missing = true;
      for (const r of await heldOf(c, 'page')) await r.fulfill({ status: 404, json: { error: 'wiki: page not found' } });
    },
    async Retry(c) {
      await main(c).locator('.error-note').filter({ hasNot: c.page.locator('.wk-src') })
        .getByRole('button', { name: 'Retry' }).click();
    },
    SourceLoaded: (c) => release(c, 'source'),
    SourceFail: (c) => failHeld(c, 'source'),
    SourceRetry: (c) => main(c).locator('.wk-src').getByRole('button', { name: 'Retry' }).click(),

    async OpenPage(c) {
      moving(c, 'page');
      if ((await routeKind(c)) === 'index') await main(c).locator('a.wk-row', { hasText: 'Ports' }).click();
      else await main(c).getByRole('button', { name: 'topics/bough/gate.md' }).click();
    },
    async FollowLink(c) {
      moving(c, 'page');
      await main(c).locator('.wk-doc .wk-links a').first().click();
    },
    async ToReview(c) {
      moving(c, 'review');
      await main(c).locator('.wk-link-bad').click();
    },
    async ToActivity(c) {
      moving(c, 'activity');
      await head(c).getByRole('button', { name: 'Activity' }).click();
    },
    async CrumbIndex(c) {
      moving(c, 'index');
      await head(c).getByRole('button', { name: 'Wiki', exact: true }).click();
    },
    async ReviewOpen(c) {
      moving(c, 'page', true);
      await main(c).getByRole('button', { name: 'Open the newer entry' }).click();
    },
    async OpenCite(c) {
      c.hold.add('source');
      await main(c).locator('.wk-doc button.wk-cite').first().click();
    },
    // Escape with nothing else owning the key closes the pane.
    CloseSource: (c) => c.page.keyboard.press('Escape'),
    async Escape(c) {
      const pane = main(c).locator('.wk-src');
      const had = (await pane.count()) > 0;
      await c.page.keyboard.press('Escape');
      await c.page.locator('.pal').waitFor({ state: 'detached' });
      if (had && (await pane.count()) === 0) c.escapeUnderDialog = true;
    },

    async Edit(c) {
      if (await narrow(c)) await main(c).getByRole('menuitem', { name: 'Edit' }).click();
      else await head(c).getByRole('button', { name: 'Edit', exact: true }).click();
    },
    async History(c) {
      const close = main(c).getByRole('button', { name: 'Close history' });
      if (await close.isVisible()) await close.click();
      else if (await main(c).locator('.wk-history').count()) {
        // On a phone with the cite pane up the doc, and the section's X
        // with it, is hidden (the spec's EditorVisible gap): the menu's
        // "Hide history" is the one way left, and it leaves the menu shut.
        await head(c).getByRole('button', { name: 'Page actions' }).click();
        await main(c).getByRole('menuitem', { name: 'Hide history' }).click();
      }
      else if (await narrow(c)) await main(c).getByRole('menuitem', { name: 'History' }).click();
      else await head(c).getByRole('button', { name: 'History', exact: true }).click();
    },
    ToggleMenu: (c) => head(c).getByRole('button', { name: 'Page actions' }).click(),
    async Resize(c) { await c.page.setViewportSize((await narrow(c)) ? WIDE : NARROW); },
    async Save(c) {
      c.hold.add('save');
      await main(c).getByRole('button', { name: 'Save', exact: true }).click();
    },
    SaveOk: (c) => release(c, 'save'),
    SaveFail: (c) => failHeld(c, 'save'),
    Cancel: (c) => main(c).getByRole('button', { name: 'Cancel', exact: true }).click(),

    async Act(c) {
      c.hold.add('claim');
      await ports(c).getByRole('button', { name: 'Mark as inference' }).click();
    },
    ActOk: (c) => release(c, 'claim'),
    ActFail: (c) => failHeld(c, 'claim'),
    SearchHistory: (c) => ports(c).getByRole('button', { name: 'Search history' }).click(),

    SearchMissing: (c) => main(c).getByRole('button', { name: 'Search the wiki' }).click(),
    CmdK: (c) => c.page.keyboard.press('ControlOrMeta+k'),
    PickHit: openHit,

    async IngestNow(c) {
      c.ingest = 'ok';
      c.note = 'started';
      await head(c).getByRole('button', { name: 'Ingest now' }).click();
    },
    async IngestFail(c) {
      c.ingest = 'fail';
      c.note = 'failed';
      await head(c).getByRole('button', { name: 'Ingest now' }).click();
    },

    // An ingest writes the claim back; the index's 15 s poll shows it.
    async FlagLands(c) {
      fs.writeFileSync(portsFile(c), PORTS);
      await c.page.clock.fastForward(15_000);
    },
  },

  read: readUiState,
  status: (c) => head(c),
  sessions: () => [],
  expectedError: (c, text) => c.missing && /status of 404/.test(text),
  invariants: (c, where) => uiInvariants(c.page, {
    // The palette is its own surface, walked by its own spec.
    include: ['main.app-main'],
    text: 'main.app-main .page-head *, main.app-main [role=status], .pal [role=status]',
  }, where),
});
