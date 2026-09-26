// go/tests/model/specs/search_scope_vs_project_deletion.fizz in the
// browser: one session, filed into one project, that the sidebar's
// full-text filter (web/src/app.tsx useFullText, the "#q" box) keeps
// naming while the project underneath it is deleted, and possibly
// recreated under the same slug.
//
// The Go twin is go/tests/model/mbt/search_scope_vs_project_deletion_test.go.
// There the "opaque round trip" is one GET of /api/search; here it is the
// filter's own debounce (180ms) plus the list poll (4s, POLL_MS in
// app.tsx) that refreshes each row's project before the row is judged: a
// query is answered, in the page's own terms, once both have settled, so
// Answer moves the clock 4.5s and waits for both requests, then reads the
// row's project once. `query` and `label` are cached into the context
// right there rather than re-derived by every later readUiState call: a
// project deleted after the query answered must not make an already
// shown hit relabel itself, any more than the server re-resolves it
// (that is the whole point of the flow), and the sidebar's own list poll
// would otherwise do exactly that the next time anything reads state.
//
// Deleting and recreating the project run through the API exactly as
// go/tests/model/mbt does: nothing about DeleteProject or RecreateProject
// has a surface on the sidebar the walk is on, and driving them from
// elsewhere in the app just to click a button would test that surface,
// not this one. `disk` is confirmed the same way, once per call, against
// the Projects page on a tab of its own (`probe`) — the one place the
// project's existence has a DOM at all — then cached too.
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

// Long enough to clear both the filter's 180ms debounce and app.tsx's
// list poll: 4s normally, 12s while a session is open (the decoy stays
// open until OpenHit, so the walk is always in the slower case).
const ANSWER_MS = 12_500;

interface Ctx {
  page: Page;
  probe: Page;
  serve: Serve;
  id: string;
  slug: string;
  name: string;
  word: string;
  disk: string;
  query: string;
  label: string;
  opened: string;
}

let pathSeq = 0;

const row = (c: Ctx) => c.page.locator(`.sidebar button.row[data-id="${c.id}"]`).first();

/** Reveals #q if the filter is folded; a no-op once it is open. */
async function openFilter(c: Ctx): Promise<void> {
  const box = c.page.locator('#q');
  if (await box.count()) return;
  await c.page.getByRole('button', { name: 'Filter the list' }).click();
  await box.waitFor();
}

/** section.proj for the walk's project, on the Projects page's own tab. */
async function diskFromProjects(c: Ctx): Promise<string> {
  const p = c.probe;
  // A fresh load every time: a same-hash goto (checked twice running) is
  // not a navigation and fires no new /api/projects request.
  await p.goto('about:blank');
  const got = p.waitForResponse((r) => new URL(r.url()).pathname === '/api/projects');
  await p.goto(`${c.serve.url}/#/projects`);
  await got;
  const sec = p.locator(`section.proj:has(a[href="#/projects/${c.slug}"])`);
  return (await sec.count()) > 0 ? 'ok' : 'none';
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  return { query: c.query, label: c.label, disk: c.disk, opened: c.opened };
}

modelTests<Ctx>({
  spec: 'search_scope_vs_project_deletion',
  role: 'Search#0',
  config: CONTROL_CONFIG,
  shared: true,
  reset: true,
  // Ask/Reask/Answer drive the clock themselves; a read that moved it on
  // its own would answer a query the path says is still loading.
  pollStepMs: 0,

  async init(page, serve) {
    const n = ++pathSeq;
    const word = `zerqol${process.pid}${n}`;
    const name = `Alpha ${process.pid}${n}`;
    const dir = controlDir(serve.home);
    queue(dir, `init${n}`, { mode: 'ok', text: `the ${word} sighting` });
    const id = await serve.newSession(`log a sighting ${n}`);
    await expectDone(serve, id);

    const pr = await serve.api.post('/api/projects', { data: { name } });
    if (!pr.ok()) throw new Error(`new project: ${pr.status()} ${await pr.text()}`);
    const slug = ((await pr.json()) as { project: { slug: string } }).project.slug;
    const asg = await serve.api.post(`/api/sessions/${id}/project`, { data: { project: slug } });
    if (!asg.ok()) throw new Error(`assign project: ${asg.status()} ${await asg.text()}`);

    // "/" alone opens the most recent session itself: a decoy to land on
    // keeps the walk's own session closed until OpenHit.
    const decoy = await serve.newSession();
    const probe = await page.context().newPage();
    const c: Ctx = { page, probe, serve, id, slug, name, word, disk: 'ok', query: 'idle', label: '', opened: '' };
    const got = page.waitForResponse((r) => new URL(r.url()).pathname === '/api/sessions');
    await page.goto(`${serve.url}/#/s/${decoy}`);
    await got;
    await page.locator('.sidebar').waitFor();
    await page.clock.pauseAt(Date.now() + 1_000);
    return c;
  },

  actions: {
    // useFullText sets its state to "loading" synchronously, before the
    // debounce's timer has run: the DOM already says so once the fill
    // itself has landed, no clock move needed.
    async Ask(c) {
      await openFilter(c);
      await c.page.locator('#q').fill(c.word);
      await (row(c)).waitFor({ state: 'hidden', timeout: 500 }).catch(() => undefined);
      c.query = 'loading';
      c.label = '';
    },

    // A fresh needle: filling the same text again is not a change React
    // sees, and the filter's debounce (and so "loading") never restarts.
    async Reask(c) {
      await openFilter(c);
      await c.page.locator('#q').fill('');
      await c.page.locator('#q').fill(c.word);
      c.query = 'loading';
      c.label = '';
    },

    // The round trip settles: the debounced /api/search, and the list
    // poll that brings this row's project up to date before it is
    // judged. The label read here is exactly what the row shows at this
    // instant, and nothing later re-reads it.
    async Answer(c) {
      // The list poll skips a run while its tab is not the front one
      // (app.tsx: "if (!document.hidden) load()"), which the probe tab
      // otherwise leaves it as.
      await c.page.bringToFront();
      const search = c.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/search');
      const sessions = c.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/sessions');
      await c.page.clock.fastForward(ANSWER_MS);
      await search;
      await sessions;
      const r = row(c);
      await r.waitFor({ state: 'visible' });
      const proj = await r.evaluate((el) => el.closest('.ws')?.querySelector('.ws-head')?.getAttribute('data-project') ?? '');
      c.query = 'shown';
      c.label = proj === c.slug ? 'alpha' : 'gone';
    },

    // As in go/tests/model/mbt: nothing about the search gates this, and
    // nothing on the sidebar shows it happening, so it runs the way any
    // other client would make it happen.
    async DeleteProject(c) {
      const res = await c.serve.api.delete(`/api/projects/${c.slug}`);
      if (!res.ok()) throw new Error(`delete project: ${res.status()} ${await res.text()}`);
      if ((await diskFromProjects(c)) !== 'none') throw new Error('project still listed after delete');
      c.disk = 'none';
    },

    async RecreateProject(c) {
      const res = await c.serve.api.post('/api/projects', { data: { name: c.name } });
      if (!res.ok()) throw new Error(`recreate project: ${res.status()} ${await res.text()}`);
      const slug = ((await res.json()) as { project: { slug: string } }).project.slug;
      if (slug !== c.slug) throw new Error(`recreated project got slug ${slug}, want the reused ${c.slug}`);
      if ((await diskFromProjects(c)) !== 'ok') throw new Error('project not listed after recreate');
      c.disk = 'recreated';
    },

    // The click-through: the session opens whatever its stale label said.
    async OpenHit(c) {
      await row(c).click();
      await c.page.waitForURL((u) => u.hash === `#/s/${c.id}`);
      c.opened = 'session';
    },
  },

  read: readUiState,
  status: (c) => c.page.locator('.sidebar'),
  // Search's own transitions (Ask, Answer, Reask, DeleteProject,
  // RecreateProject) never touch the session's transcript — the one
  // thing the search finds is a fixed line from Init — so there is
  // nothing here for the trace check to replay (like a ui_* flow, per
  // helpers/model.ts's saveTranscripts).
  sessions: () => [],
  async cleanup(c) {
    await c.probe.close().catch(() => undefined);
  },
});

/** Waits for a fresh session's first (queued, non-blocking) turn to close. */
async function expectDone(serve: Serve, id: string): Promise<void> {
  const deadline = Date.now() + 20_000;
  for (;;) {
    const res = await serve.api.get(`/api/sessions/${id}`);
    if (res.ok()) {
      const row = ((await res.json()) as { session?: { status?: string } }).session;
      if (row?.status === 'done') return;
    }
    if (Date.now() > deadline) throw new Error(`session ${id} did not finish its first turn`);
    await new Promise((r) => setTimeout(r, 50));
  }
}
