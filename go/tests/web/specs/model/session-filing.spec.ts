// Filing sessions into projects, walked in the browser (recipe:
// go/tests/model/README.md): go/tests/model/specs/session_filing.fizz.
// Every generated path is driven through the page against a real serve
// whose model is llm-control, and at each node the whole abstract state
// (both sessions and project "b") must read the same off the DOM.
//
// Not modelTests(): this spec has three roles, not one (the local
// session, the project session and project "b"); Assign's target is the
// runner's `oneof`, which only the next state names; and 81 paths want
// the worker's serve (sharedServe) rather than a boot each. So the walk
// is here, with helpers/model.ts's paths and the same per-node checks.
// Each path works in projects and sessions of its own, never in the
// whole list, so walks sharing a serve cannot see each other.
import { execFileSync } from 'child_process';
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, waitTaken } from '../../helpers/control';
import { loadPaths, type Step, walkTest } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

const SPEC = 'session_filing';

interface Ctx {
  page: Page;
  serve: Serve;
  key: string;     // this walk's suffix: its titles, repo and projects
  local: string;   // Session#0
  orb: string;     // Session#1
  repo: string;    // the local session's checkout, its by-repo group
  a: { slug: string; name: string };
  b: { slug: string; name: string };  // made by FromRepo, typed into its dialog
  turn: number;
}

const title = (c: Ctx, id: string) => `${id === c.local ? 'local' : 'orb'} ${c.key}`;

// A history file as serve lists it; the rename keeps serve from reading
// half of one. The Go adapter seeds the same two.
function seed(serve: Serve, meta: Record<string, unknown>, rest: { kind: string; data: object }[] = []): string {
  const id = crypto.randomUUID();
  const at = new Date().toISOString();
  const lines = [{ kind: 'meta', data: meta }, ...rest].map((e, i) => JSON.stringify({ seq: i + 1, at, ...e }));
  const dir = path.join(serve.home, '.bough', 'history');
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, id + '.seed'), lines.join('\n') + '\n');
  fs.renameSync(path.join(dir, id + '.seed'), path.join(dir, id + '.jsonl'));
  return id;
}

async function ok(res: Awaited<ReturnType<Serve['api']['post']>>, what: string): Promise<void> {
  if (!res.ok()) throw new Error(`${what}: ${res.status()} ${await res.text()}`);
}

async function init(page: Page, serve: Serve, i: number): Promise<Ctx> {
  const key = `${i}x${crypto.randomBytes(3).toString('hex')}`;
  const c: Ctx = {
    page, serve, key, local: '', orb: '', repo: `filing${key}`, turn: 0,
    a: { slug: '', name: `Filing A ${key}` },
    b: { slug: `filing-b-${key}`, name: `Filing B ${key}` },
  };
  // Project "a" exists for the whole walk, as the spec assumes.
  const res = await serve.api.post('/api/projects', { data: { name: c.a.name } });
  await ok(res, 'create project a');
  c.a.slug = (await res.json()).project.slug;
  // The local session has never run a turn and has no child; its cwd
  // names the repo by-repo groups it under. The project session was made
  // for "a" and never needs its orb: nothing in the spec starts it.
  const cwd = path.join(serve.home, 'repos', c.repo);
  fs.mkdirSync(cwd, { recursive: true });
  fs.mkdirSync(path.join(serve.home, 'orb'), { recursive: true });
  c.local = seed(serve, { cwd, mode: 'local', origin: 'web' });
  c.orb = seed(serve, { cwd: path.join(serve.home, 'orb'), mode: 'project', project: c.a.slug, origin: 'web' },
    [{ kind: 'input', data: { text: 'work in a' } }, { kind: 'done', data: {} }]);
  // Titles to find the rows by on pages that carry no ids.
  for (const id of [c.local, c.orb]) await ok(await serve.api.post(`/api/sessions/${id}/rename`, { data: { title: title(c, id) } }), 'rename');
  await page.goto(`${serve.url}/#/s/${c.local}`);
  return c;
}

/* ---------------- reading the page ---------------- */

async function go(c: Ctx, hash: string): Promise<void> {
  if (await c.page.evaluate(() => location.hash) !== hash) await c.page.evaluate((h) => { location.hash = h; }, hash);
}

const sidebarRow = (c: Ctx, id: string) => c.page.locator(`.sidebar button.row[data-id="${id}"]`).first();

// A project's slug as the spec names it.
const abstract = (c: Ctx, slug: string) => (slug === c.a.slug ? 'a' : slug === c.b.slug ? 'b' : slug ? `other:${slug}` : '');

/** The sidebar group the row sits in: a project group's head carries its slug. */
async function group(c: Ctx, id: string): Promise<string> {
  const r = sidebarRow(c, id);
  await r.waitFor({ timeout: 3000 });
  return abstract(c, await r.evaluate((el) => el.closest('.ws')?.querySelector('.ws-head')?.getAttribute('data-project') ?? ''));
}

/**
 * What the session's running child was started with, from its Settings:
 * a live local session says whose MEMORY.md it runs with, and nothing is
 * said of one with no child. live, injected.
 */
async function brief(c: Ctx, id: string): Promise<[boolean, string]> {
  await go(c, `#/s/${id}`);
  const btn = c.page.getByRole('button', { name: 'Session settings' });
  await btn.click({ timeout: 3000 });
  const pop = c.page.getByRole('dialog', { name: 'Session settings' });
  await pop.waitFor({ timeout: 3000 });
  // "Running with <name>’s MEMORY.md." or "Running with no project MEMORY.md.", then what a move waits for.
  const note = pop.getByRole('note');
  let out: [boolean, string] = [false, ''];
  if (await note.count()) {
    const text = (await note.textContent()) ?? '';
    const had = /^Running with (.+?)’s MEMORY\.md\./.exec(text)?.[1];
    out = [true, had === undefined ? (text.startsWith('Running with no project MEMORY.md.') ? '' : `unread: ${text}`)
      // A deleted project has no name left to show, only its slug.
      : had === c.a.name ? 'a' : had === c.b.name || had === c.b.slug ? 'b' : `other: ${had}`];
  }
  await btn.click();
  return out;
}

/** The project page's thread list, every fold opened: which of the walk's sessions it lists. */
async function pageThreads(c: Ctx, slug: string): Promise<string[]> {
  await go(c, `#/projects/${slug}`);
  await c.page.locator('.prj-queue').waitFor({ timeout: 3000 });
  for (const more of await c.page.locator('.prj-queue .prj-more').all()) await more.click();
  const labels = await c.page.locator('.prj-queue .prj-thread').evaluateAll((els) => els.map((e) => e.getAttribute('aria-label') ?? ''));
  return [c.local, c.orb].filter((id) => labels.some((l) => l.startsWith(title(c, id) + ',')));
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  // The Projects page: whether "b" is a project, and which sessions sit
  // in the unfiled pile under a detected repo.
  await go(c, '#/projects');
  await page.locator('.proj-body').waitFor({ timeout: 3000 });
  await expect(page.getByText('Reading repos…')).toHaveCount(0, { timeout: 3000 });
  for (const fold of await page.locator('.proj-empty[aria-expanded="false"]').all()) await fold.click();
  const made = (await page.locator('section.proj h2', { hasText: c.b.name }).count()) > 0;
  const byrepo = async (id: string) => (await page.locator('.rp-repo', { has: page.locator('.rp-repo-name', { hasText: c.repo }) })
    .locator(`.proj-open[aria-label^="${title(c, id)},"]`).count()) > 0;
  const pile = { [c.local]: await byrepo(c.local), [c.orb]: await byrepo(c.orb) };

  // The project pages; one that does not exist has no page to open.
  const onA = await pageThreads(c, c.a.slug);
  const onB = made ? await pageThreads(c, c.b.slug) : [];
  const pageOf = (id: string) => [onA.includes(id) && 'a', onB.includes(id) && 'b'].filter(Boolean).join('+');

  // Each session's Settings, ending on the local one: open, it is listed
  // in the sidebar whatever it holds, so both rows' groups read there.
  const [orbLive, orbInjected] = await brief(c, c.orb);
  const [live, injected] = await brief(c, c.local);
  const project = { [c.local]: await group(c, c.local), [c.orb]: await group(c, c.orb) };
  return {
    'Session#0.project': project[c.local], 'Session#0.page': pageOf(c.local), 'Session#0.byrepo': pile[c.local],
    'Session#0.live': live, 'Session#0.injected': injected,
    'Session#1.project': project[c.orb], 'Session#1.page': pageOf(c.orb), 'Session#1.byrepo': pile[c.orb],
    'Session#1.live': orbLive, 'Session#1.injected': orbInjected,
    'Project#0.made': made,
  };
}

/* ---------------- the actions ---------------- */

const nameOf = (c: Ctx, p: string) => (p === 'a' ? c.a.name : p === 'b' ? c.b.name : 'Unassigned');

// The local session's child: the one bough process resumed with its id.
function childPid(id: string): number {
  const out = execFileSync('ps', ['-A', '-ww', '-o', 'pid=,args=']).toString();
  const pids = out.split('\n').filter((l) => l.includes(' --headless ') && l.includes(` -r ${id}`)).map((l) => parseInt(l.trim(), 10));
  if (pids.length !== 1) throw new Error(`want one child resumed with -r ${id}, found ${JSON.stringify(pids)}`);
  return pids[0];
}

async function settingsProject(c: Ctx, to: string): Promise<void> {
  await go(c, `#/s/${c.local}`);
  const btn = c.page.getByRole('button', { name: 'Session settings' });
  await btn.click();
  const pop = c.page.getByRole('dialog', { name: 'Session settings' });
  await pop.getByRole('combobox', { name: /^Project:/ }).click();
  await c.page.getByRole('option', { name: nameOf(c, to), exact: true }).click();
  await expect(pop.getByRole('combobox', { name: /^Project:/ })).toHaveAccessibleName(`Project: ${nameOf(c, to)}`);
  await btn.click();
}

const actions: Record<string, (c: Ctx, next: Record<string, unknown>) => Promise<void>> = {
  // Settings > Project, to wherever the runner's `oneof` went.
  Assign: (c, next) => settingsProject(c, String(next['Session#0.project'])),

  // The row dragged onto "a"'s sidebar group.
  async Drop(c) {
    await go(c, `#/s/${c.local}`);
    await sidebarRow(c, c.local).dragTo(c.page.locator('.sidebar .ws', { has: c.page.locator(`.ws-head[data-project="${c.a.slug}"]`) }));
  },

  // "Create project" on the repo's unfiled group, named so it is "b".
  async FromRepo(c) {
    await go(c, '#/projects');
    const grp = c.page.locator('.rp-repo', { has: c.page.locator('.rp-repo-name', { hasText: c.repo }) });
    await grp.getByRole('button', { name: 'Create project' }).click();
    const dlg = c.page.getByRole('dialog');
    await dlg.locator('input').fill(c.b.name);
    await dlg.getByRole('button', { name: 'Create and move' }).click();
    await expect(dlg).toHaveCount(0);
  },

  // No page offers a move of a project session; another client sends it.
  async ApiMove(c) {
    const res = await c.serve.api.post(`/api/sessions/${c.orb}/project`, { data: { project: '' } });
    expect(res.status(), 'moving a project session out of its project').toBe(409);
  },

  // A message through the composer resumes the child.
  async Start(c) {
    await go(c, `#/s/${c.local}`);
    const name = `f${c.key}-${String(++c.turn).padStart(3, '0')}`;
    const dir = controlDir(c.serve.home);
    queue(dir, name, { mode: 'ok', text: `done ${name}` });
    await c.page.locator('#composer').fill(`turn ${name}`);
    await c.page.getByRole('button', { name: 'Send', exact: true }).click();
    await waitTaken(dir, name);
  },

  // Nothing a person can press stops an idle local child: a crash.
  async Stop(c) {
    process.kill(childPid(c.local), 'SIGKILL');
  },

  // Delete… in "b"'s menu on the Projects page, typed to confirm.
  async Delete(c) {
    await go(c, '#/projects');
    await c.page.getByRole('button', { name: `More actions for ${c.b.name}` }).click();
    await c.page.getByRole('menuitem', { name: 'Delete…' }).click();
    const dlg = c.page.getByRole('dialog');
    await dlg.locator('input').fill(c.b.slug);
    await dlg.getByRole('button', { name: 'Delete project' }).click();
    await expect(dlg).toHaveCount(0);
    // The delete is done when the page stops listing it (act awaits the
    // DELETE, then refreshes); before that a read would open a page that
    // no longer exists.
    await expect(c.page.locator('section.proj h2', { hasText: c.b.name })).toHaveCount(0);
  },
};

/* ---------------- the walk ---------------- */

// The spec's state without its role references ("local": "role Session#0").
const expected = (s: Record<string, unknown>) => Object.fromEntries(Object.entries(s).filter(([k]) => k.includes('.')));

// helpers/model.ts's per-node checks: no sideways scroll, the carrier on
// screen, nothing logged as an error.
async function invariants(page: Page, carrier: ReturnType<Page['locator']>, errors: string[], where: string): Promise<void> {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(carrier, `${where}: status not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

function saveTranscript(serve: Serve, id: string, title: string): void {
  const root = process.env.MODEL_TRACE_DIR;
  if (!root) return;
  const dir = path.join(root, SPEC);
  fs.mkdirSync(dir, { recursive: true });
  const src = path.join(serve.home, '.bough', 'history', id + '.jsonl');
  const slug = title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 120);
  if (fs.existsSync(src)) fs.copyFileSync(src, path.join(dir, `${slug}-${id}.jsonl`));
}

// Top level: a worker option cannot differ between groups of one file.
test.use({ workerServeOpts: { config: CONTROL_CONFIG } });
// A click that finds nothing fails in seconds, inside the state poll,
// instead of hanging the read until the test's own timeout.
test.use({ actionTimeout: 5_000 });

test.describe(`model: ${SPEC}`, () => {
  // A read visits four pages; a six-step path is a couple dozen of them.
  test.describe.configure({ timeout: 180_000 });

  const bare = (a: string) => a.slice(a.indexOf('.') + 1);
  loadPaths(SPEC).forEach((trace: Step[], i) => {
    const walk = trace.slice(1).map((s) => s.action).join(' → ');
    walkTest(SPEC)(`path ${i}: ${walk}`, async ({ sharedServe: serve, page }, info) => {
      const errors: string[] = [];
      page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e)));
      // The list poll runs on a clock the walk moves (see helpers/model.ts).
      await page.clock.install();
      const c = await init(page, serve, i);
      try {
        for (const [n, step] of trace.entries()) {
          const where = `step ${n} (${step.action})`;
          if (n > 0) {
            const act = actions[bare(step.action)];
            if (!act) throw new Error(`${SPEC}: no action for ${step.action}`);
            await act(c, step.state);
          }
          await expect.poll(async () => {
            await page.clock.fastForward(5_000);
            // A read that could not finish is a wrong state, retried like
            // one, and the diff says where it stopped.
            try { return await readUiState(c); } catch (e) { return { unread: `${page.url()}: ${String(e).split('\n')[0]}` }; }
          }, { message: `${where}: state`, timeout: 20_000 }).toEqual(expected(step.state));
          await invariants(page, sidebarRow(c, c.local), errors, where);
        }
      } finally {
        // The next walk's sessions are the only live ones.
        try { process.kill(childPid(c.local), 'SIGKILL'); } catch { /* no child */ }
        saveTranscript(serve, c.local, info.title);
      }
    });
  });
});
