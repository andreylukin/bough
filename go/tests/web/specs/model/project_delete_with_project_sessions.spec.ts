// go/tests/model/specs/project_delete_with_project_sessions.fizz in the
// browser: a project whose one repo is a remote, its main thread and one
// thread (both mode=project, on the fake container runtime), the project
// deleted from the Projects page and its slug created again. The Go twin
// is go/tests/model/mbt/project_delete_with_project_sessions_test.go; the
// serve and the git side are set up exactly as there, and each path takes
// its own project and repo on the worker's one serve.
//
// The spec reads several copies of the same membership at different
// times, and each is a tab of its own:
//
//   page     (the walk's tab) the control room's sidebar, its clock
//            paused: it regroups only at SidebarPoll (sidebar).
//   pageTab  #/projects/<slug> as last opened, paused (page).
//   repoTab  the Projects page as last opened, paused: its Unassigned
//            groups are the last by-repo read (byrepo), and its "Create
//            project" is ProjectFromRepo.
//   probe    a tab opened afresh at every read: what the server holds now
//            (disk, sessions, thread, row, orbs).
//   actor    where the person does everything else: messages the
//            project, starts the thread, deletes, creates.
//   sendTab  the old thread, where the person's message is held in the
//            browser between PersonSendsToOldThread and SendAnswered.
//
// A tab the spec says has not read in this incarnation (page, byrepo)
// is blanked (about:blank) where the spec resets it.
//
// What has no surface on any page is read the way the Go adapter reads
// it, and says so where it is read: meta.json's Project (meta), the cache
// clone and the remote (cache, lost). The DELETE is one request whose
// steps nothing outside can see: as in Go, the page is read before and
// after it and each phase reports the fields its step changes from the
// read after, the rest from the read before. gen, send and the from-repo
// counts are what the walk saw the page do at that step.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { controlDir, queue, release } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

const CONFIG = '- id: llm\n  plugin: llm-control\n' +
  '- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n';

const WAIT_MS = 30_000;

// Up to twenty steps, several of them session boots and model turns.
test.describe.configure({ timeout: 240_000 });

interface Truth { disk: string; sessions: string; thread: string; meta: string; row: string; orbs: string; cache: string; lost: boolean }

interface Ctx {
  page: Page;
  probe: Page;
  actor: Page;
  pageTab: Page;
  repoTab: Page;
  sendTab?: Page;
  serve: Serve;
  dir: string;
  tag: string;
  name: string; slug: string;
  repo: string; origin: string;
  gen: number;
  main: string; thread: string;
  held: string;
  commits: string[];
  phase: string;
  pre?: Truth; post?: Truth;
  send: string;
  sendRoute?: Route;
  listed: number; moved: number; refused: number;
  turn: number;
  errors: string[];
}

let pathSeq = 0;

// --- serve's files and git, for what no page shows

function git(dir: string, ...args: string[]): string {
  // No config but the test's own: the host's (signing, hooks, identity) is not the test's.
  return execFileSync('git', ['-C', dir, '-c', 'user.name=agent', '-c', 'user.email=agent@example.invalid', '-c', 'commit.gpgsign=false', ...args], {
    env: { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null' },
    encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

const gitOk = (dir: string, ...args: string[]) => { try { git(dir, ...args); return true; } catch { return false; } };

interface Meta { sessions?: Record<string, { project?: string }>; mains?: Record<string, string> }

function meta(c: Ctx): Meta {
  try { return JSON.parse(fs.readFileSync(path.join(c.serve.home, '.bough', 'serve', 'meta.json'), 'utf8')) as Meta; }
  catch { return {}; }
}

const histPath = (c: Ctx, id: string) => path.join(c.serve.home, '.bough', 'history', id + '.jsonl');
const cacheDir = (c: Ctx) => path.join(c.serve.home, '.bough', 'orbs', 'cache', c.slug, c.repo + '.git');

interface Entry { kind: string; data?: Record<string, unknown> }

function entries(c: Ctx, id: string): Entry[] {
  try {
    return fs.readFileSync(histPath(c, id), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l) as Entry);
  } catch { return []; }
}

function turnOpen(es: Entry[]): boolean {
  let open = false;
  for (const e of es) {
    if (e.kind === 'input') open = true;
    else if (e.kind === 'done' || e.kind === 'cancelled') open = false;
  }
  return open;
}

const closedTurns = (es: Entry[]) => es.filter((e) => e.kind === 'done').length;

/** Reports of the thread's turns that main woke on. */
const notices = (c: Ctx) => entries(c, c.main).filter((e) => e.kind === 'input' && e.data?.reason === 'notice' && String(e.data?.text ?? '').includes(c.thread)).length;

/** The thread's checkout of the repo, from its orb's state.json. */
function worktree(c: Ctx, id: string): string {
  try {
    const st = JSON.parse(fs.readFileSync(path.join(c.serve.home, '.bough', 'orbs', id, 'state.json'), 'utf8')) as { worktrees?: Record<string, string> };
    return st.worktrees?.[c.repo] ?? '';
  } catch { return ''; }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until(what: string, f: () => boolean | Promise<boolean>): Promise<void> {
  const deadline = Date.now() + WAIT_MS;
  while (!(await f())) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}`);
    await sleep(20);
  }
}

/** Main has woken on a report of every turn the thread closed, and that turn of main's closed. */
const reported = (c: Ctx) => until(`main ${c.main} to report the thread`, () => {
  const n = closedTurns(entries(c, c.thread));
  return notices(c) >= n && !turnOpen(entries(c, c.main));
});

function queueTurn(c: Ctx, mode: 'ok' | 'block', text: string): string {
  const name = `${c.tag}-p${String(++c.turn).padStart(4, '0')}`;
  queue(c.dir, name, { mode, text });
  return name;
}

/** meta and cache/lost: serve's meta.json and git, which no page shows. */
function files(c: Ctx): Pick<Truth, 'meta' | 'cache' | 'lost'> {
  let m = '';
  if (c.main) {
    const s = meta(c).sessions ?? {};
    const one = (id: string) => (s[id]?.project === c.slug ? 'alpha' : s[id]?.project ? `in ${s[id]?.project}` : '');
    m = one(c.main) === one(c.thread) ? one(c.main) : `main ${one(c.main)}, thread ${one(c.thread)}`;
  }
  const inOrigin = (sha: string) => gitOk(c.origin, 'cat-file', '-e', `${sha}^{commit}`);
  let cache = 'none';
  const gd = cacheDir(c);
  if (fs.existsSync(gd)) {
    cache = 'clean';
    for (const sha of git(gd, 'for-each-ref', '--format=%(objectname)', 'refs/heads/bough/').split(/\s+/).filter(Boolean)) {
      if (!inOrigin(sha)) cache = 'unpushed';
    }
  }
  const lost = c.commits.some((sha) => !inOrigin(sha) && !gitOk(gd, 'cat-file', '-e', `${sha}^{commit}`));
  return { meta: m, cache, lost };
}

// --- the pages

/** Loads url in tab from scratch (a hash change alone reloads nothing) and waits for what it reads first. */
async function open(tab: Page, url: string, reads: string[]): Promise<void> {
  await tab.goto('about:blank');
  const got = Promise.all(reads.map((u) => tab.waitForResponse((r) => new URL(r.url()).pathname === u, { timeout: WAIT_MS })));
  await tab.goto(url);
  await got;
}

/** A paused tab: no poll runs unless the walk moves its clock. */
async function paused(tab: Page): Promise<void> {
  await tab.clock.install();
  await tab.clock.pauseAt(Date.now() + 1_000);
}

/** Where a sidebar lists a session: under this project, another, a folder ("loose"), or nowhere ("none"). */
async function listedIn(c: Ctx, tab: Page, id: string): Promise<string> {
  return tab.evaluate(([id, slug]) => {
    const row = document.querySelector(`.sidebar button.row[data-id="${id}"]`);
    if (!row) return 'none';
    const p = row.closest('.ws')?.querySelector('.ws-head')?.getAttribute('data-project');
    return p === slug ? 'alpha' : p ? `in ${p}` : 'loose';
  }, [id, c.slug] as const);
}

/**
 * Which group of a sidebar the walk's two sessions sit in: "none" |
 * "alpha" | "loose", or what else it saw. Outside a project the thread is
 * not a row of its own: a spawned session lists under its parent's row,
 * whose label counts its running agents (app.tsx, "A background agent is
 * not a row of its own"), so it is wherever main is.
 */
async function grouping(c: Ctx, tab: Page): Promise<string> {
  if (!c.main) return 'none';
  const main = await listedIn(c, tab, c.main);
  let thread = await listedIn(c, tab, c.thread);
  if (thread === 'none' && main !== 'alpha' && main !== 'none') thread = main;
  return main === thread ? main : `main ${main}, thread ${thread}`;
}

/** Whether the thread is in a turn, by its own row or by main's count of its running agents. */
async function threadState(c: Ctx, tab: Page): Promise<string> {
  const own = tab.locator(`.sidebar button.row[data-id="${c.thread}"]`);
  if (await own.count()) return /\brunning\b/i.test((await own.getAttribute('aria-label')) ?? '') ? 'running' : 'idle';
  const main = (await tab.locator(`.sidebar button.row[data-id="${c.main}"]`).getAttribute('aria-label')) ?? '';
  return /background: 1 running/.test(main) ? 'running' : 'idle';
}

/** The server's side as a fresh tab shows it, plus the files no tab shows. */
async function truth(c: Ctx): Promise<Truth> {
  const p = c.probe;
  // The Projects page with this project's Orb section open: its section
  // is the project on disk, its container rows the orbs attributed to it.
  const orbRead = `/api/projects/${c.slug}/orb`;
  await open(p, `${c.serve.url}/#/projects/${c.slug}/orb`, ['/api/projects', '/api/sessions']);
  const sec = p.locator(`section.proj:has(a[href="#/projects/${c.slug}"])`);
  await sleep(150);
  const disk = (await sec.count()) > 0 ? 'ok' : 'none';
  let orbs = 'none';
  if (c.main) {
    let n = 0;
    if (disk === 'ok') {
      await expect(sec.locator('.proj-orb .orb-sessions, .proj-orb .proj-none').first(), `${orbRead} read`).toBeVisible();
      n = await sec.locator('.proj-orb .orb-row').count();
    }
    orbs = n === 2 ? 'alpha' : n === 0 ? 'gone' : `${n} orbs listed`;
  }
  let sessions = 'none', thread = 'idle';
  if (c.main) {
    await expect(p.locator(`.sidebar button.row[data-id="${c.main}"]`), 'main in the sidebar').toHaveCount(1);
    sessions = 'started';
    thread = await threadState(c, p);
  }
  const row = c.main ? await grouping(c, p) : '';
  return { disk, sessions, thread, row: row === 'alpha' ? 'alpha' : row === 'loose' ? '' : row, orbs, ...files(c) };
}

/** #/projects/<slug> as the page tab last read it. */
async function pageRead(c: Ctx): Promise<string> {
  const t = c.pageTab;
  if (t.url() === 'about:blank') return '';
  if (await t.getByText('Project not found').isVisible()) return '404';
  if (!(await t.locator('h1.prj-name').isVisible())) return 'loading';
  // A new project lists nothing of its own yet: any main, thread or orb
  // it shows was gen 1's.
  if (c.gen === 1) return 'empty';
  const main = await t.locator('.prj-main-row, .prj-main-thread').count();
  const threads = await t.locator('.prj-thread:not(.prj-main-row)').count();
  const orbs = Number((await t.locator('.prj-orbs summary .num').textContent().catch(() => '0')) ?? 0);
  return main + threads + orbs > 0 ? 'inherits' : 'empty';
}

/** The by-repo group of the repo tab's Unassigned section, when it lists this walk's repo. */
const repoGroup = (c: Ctx) => c.repoTab.locator('.rp-repo', { has: c.repoTab.locator('.rp-repo-name', { hasText: new RegExp(`^${c.repo}$`) }) });

async function byRepoRead(c: Ctx): Promise<string> {
  const t = c.repoTab;
  if (t.url() === 'about:blank') return '';
  const g = repoGroup(c);
  if (!(await g.count())) return 'empty';
  const ids = await g.locator('.proj-row').evaluateAll((els) => els.length);
  return ids === 2 ? 'orphans' : `${ids} sessions under ${c.repo}`;
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  let v: Truth;
  if (c.phase === '') v = await truth(c);
  else if (c.phase === 'ending') v = { ...c.pre! };
  else if (c.phase === 'removing') v = { ...c.pre!, thread: c.post!.thread };
  else v = { ...c.pre!, thread: c.post!.thread, disk: c.post!.disk, cache: c.post!.cache, lost: c.post!.lost };
  return {
    disk: v.disk, gen: c.gen, sessions: v.sessions, thread: v.thread, meta: v.meta, row: v.row, orbs: v.orbs,
    cache: v.cache, lost: v.lost, phase: c.phase,
    sidebar: await grouping(c, c.page), page: await pageRead(c), byrepo: await byRepoRead(c),
    listed: c.listed, moved: c.moved, refused: c.refused, send: c.send,
  };
}

// --- actions

async function projectsMenu(c: Ctx, item: string): Promise<void> {
  const a = c.actor;
  await open(a, `${c.serve.url}/#/projects`, ['/api/projects', '/api/sessions']);
  const sec = a.locator(`section.proj:has(a[href="#/projects/${c.slug}"])`);
  await sec.getByRole('button', { name: /^More actions for / }).click();
  await a.getByRole('menuitem', { name: item, exact: true }).click();
}

/** Delete… with the slug typed back; the answer to the DELETE it sends. */
async function deleteFromPage(c: Ctx): Promise<{ ok: boolean; text: string }> {
  await projectsMenu(c, 'Delete…');
  const d = c.actor.getByRole('dialog');
  await d.getByRole('textbox').fill(c.slug);
  const res = c.actor.waitForResponse((r) => new URL(r.url()).pathname === `/api/projects/${c.slug}` && r.request().method() === 'DELETE', { timeout: 120_000 });
  await d.getByRole('button', { name: 'Delete project', exact: true }).click();
  const r = await res;
  await expect(d).toBeHidden();
  return { ok: r.ok(), text: await r.text() };
}

modelTests<Ctx>({
  spec: 'project_delete_with_project_sessions',
  role: 'Project#0',
  config: CONFIG,
  shared: true,
  // SidebarPoll and ProjectPagePoll are the spec's: a read that moved the
  // clock would run them where the path says they have not happened.
  pollStepMs: 0,
  // The spec's refusals, which the page shows: the project page's 404,
  // the 409 of a delete with unpushed work, of a message to a thread whose
  // project is gone, and of moving a project session. Chromium logs each.
  allowConsole: /status of 40[49] \((Not Found|Conflict)\)/,

  async init(page, serve) {
    const tag = `w${process.pid}x${++pathSeq}`;
    const name = `Alpha ${tag}`;
    const repo = `widget${tag}`;
    const origins = path.join(serve.home, 'origins');
    const seed = path.join(origins, repo + '-seed');
    const origin = path.join(origins, repo + '.git');
    fs.mkdirSync(seed, { recursive: true });
    git(seed, 'init', '-q', '-b', 'main');
    fs.writeFileSync(path.join(seed, 'README'), 'widget\n');
    git(seed, 'add', 'README');
    git(seed, 'commit', '-q', '-m', 'seed');
    git(origins, 'clone', '-q', '--bare', seed, origin);
    // The project as it was set up before the walk: made, and pointed at the remote.
    const res = await serve.api.post('/api/projects', { data: { name } });
    if (!res.ok()) throw new Error(`new project: ${res.status()} ${await res.text()}`);
    const slug = ((await res.json()) as { project: { slug: string } }).project.slug;
    const put = await serve.api.put(`/api/projects/${slug}/orb/files/project.yml`, { data: { text: `name: ${name}\nrepos:\n  - remote: ${origin}\n` } });
    if (!put.ok()) throw new Error(`project.yml: ${put.status()} ${await put.text()}`);

    const ctx = page.context();
    const [probe, actor, pageTab, repoTab] = [await ctx.newPage(), await ctx.newPage(), await ctx.newPage(), await ctx.newPage()];
    const c: Ctx = {
      page, probe, actor, pageTab, repoTab, serve, dir: controlDir(serve.home), tag, name, slug, repo, origin,
      gen: 1, main: '', thread: '', held: '', commits: [], phase: '', send: '',
      listed: 0, moved: 0, refused: 0, turn: 0, errors: [],
    };
    // The other tabs owe what the walk's tab owes: nothing logged as an error.
    const allowed = /status of 40[49] \((Not Found|Conflict)\)/;
    for (const t of [probe, actor, pageTab, repoTab]) {
      t.on('console', (m) => { if (m.type() === 'error' && !allowed.test(m.text())) c.errors.push(m.text()); });
      t.on('pageerror', (e) => c.errors.push(String(e)));
    }
    await paused(pageTab);
    await paused(repoTab);
    await page.clock.pauseAt(Date.now() + 1_000);
    await open(page, `${serve.url}/#/`, ['/api/sessions']);
    await page.locator('.sidebar').waitFor();
    return c;
  },

  actions: {
    // A message to the project (its home composer), then main's Start
    // thread with a task; both name the repo, which is how by-repo later
    // finds them.
    async StartMainAndThread(c) {
      const a = c.actor;
      const text = `look at ~/repos/${c.repo}`;
      queueTurn(c, 'ok', 'looked');
      await open(a, `${c.serve.url}/#/projects/${c.slug}`, [`/api/projects/${c.slug}`]);
      await a.getByRole('textbox', { name: 'Message the project' }).fill(text);
      await a.locator('.prj-first').getByRole('button', { name: 'Send', exact: true }).click();
      await until(`main of ${c.slug}`, () => Boolean(meta(c).mains?.[c.slug]));
      c.main = meta(c).mains![c.slug];
      await until(`main's first turn`, () => { const es = entries(c, c.main); return es.some((e) => e.kind === 'input' && e.data?.text === text) && !turnOpen(es); });

      const task = `work in ~/repos/${c.repo}`;
      queueTurn(c, 'ok', 'worked');
      await a.getByRole('button', { name: 'Start thread' }).click();
      await a.getByPlaceholder('What should the thread do?').fill(task);
      const res = a.waitForResponse((r) => new URL(r.url()).pathname === '/api/sessions' && r.request().method() === 'POST');
      await a.getByRole('dialog').getByRole('button', { name: 'Start', exact: true }).click();
      const created = await res;
      if (!created.ok()) throw new Error(`start thread: ${created.status()} ${await created.text()}`);
      c.thread = String(((await created.json()) as { session?: { id?: string } }).session?.id ?? '');
      if (!c.thread) throw new Error('start thread: no session id');
      await until(`the thread's task`, () => { const es = entries(c, c.thread); return closedTurns(es) > 0 && !turnOpen(es); });
      await reported(c);
      await until('both worktrees', () => Boolean(worktree(c, c.main) && worktree(c, c.thread)));
    },

    // The thread's own composer.
    async ThreadRun(c) {
      const a = c.actor;
      const name = queueTurn(c, 'block', 'finished');
      await open(a, `${c.serve.url}/#/s/${c.thread}`, ['/api/sessions']);
      await a.locator('#composer').fill(`turn ${name}`);
      await a.getByRole('button', { name: 'Send', exact: true }).click();
      await until(`turn ${name} taken`, () => fs.existsSync(path.join(c.dir, name + '.taken')));
      c.held = name;
      await until('the thread in a turn', () => turnOpen(entries(c, c.thread)));
    },

    async ThreadFinish(c) {
      release(c.dir, c.held);
      c.held = '';
      await until('the thread\'s turn to end', () => !turnOpen(entries(c, c.thread)));
      await reported(c);
    },

    // The thread's agent at work in its worktree, played by git: no page is involved.
    async ThreadCommit(c) {
      const wt = worktree(c, c.thread);
      const f = `change${c.commits.length + 1}`;
      fs.writeFileSync(path.join(wt, f), f + '\n');
      git(wt, 'add', f);
      git(wt, 'commit', '-q', '-m', f);
      c.commits.push(git(wt, 'rev-parse', 'HEAD'));
    },

    async ThreadPush(c) {
      git(worktree(c, c.thread), 'push', '-q', 'origin', `bough/${c.thread}`);
    },

    // The dialog's confirm: the whole DELETE runs here, and the three
    // phase steps after it report it (see the top of the file).
    async DeleteProject(c) {
      c.pre = await truth(c);
      const r = await deleteFromPage(c);
      if (!r.ok) throw new Error(`delete: ${r.text}`);
      // EndProject cut the turn in flight with its process.
      c.held = '';
      c.post = await truth(c);
      c.phase = 'ending';
    },

    // Refused, and the page says why: the toast names the branch.
    async DeleteRefusedUnpushed(c) {
      const r = await deleteFromPage(c);
      if (r.ok) throw new Error('delete with unpushed work was not refused');
      await expect(c.actor.getByRole('alert').filter({ hasText: `bough/${c.thread}` })).toBeVisible();
      await c.actor.getByRole('button', { name: /^Dismiss/ }).click().catch(() => undefined);
    },

    async EndProject(c) { c.phase = 'removing'; },
    async RemoveDirs(c) { c.phase = 'unassign'; },
    // The spec resets the page's last read here: that incarnation is gone.
    async MetaUnassign(c) {
      c.phase = '';
      await c.pageTab.goto('about:blank');
    },

    // The list poll of the walk's tab (every 4 s).
    async SidebarPoll(c) {
      const got = c.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/sessions');
      await c.page.clock.fastForward(4_500);
      await got;
      await sleep(100);
    },

    async ProjectPagePoll(c) {
      await open(c.pageTab, `${c.serve.url}/#/projects/${c.slug}`, [`/api/projects/${c.slug}`]);
      await expect(c.pageTab.locator('h1.prj-name').or(c.pageTab.getByText('Project not found'))).toBeVisible();
    },

    // The person opens the old thread and sends; the request is held in
    // the browser until SendAnswered.
    async PersonSendsToOldThread(c) {
      if (!c.sendTab) {
        c.sendTab = await c.page.context().newPage();
        const t = c.sendTab;
        t.on('console', (m) => { if (m.type() === 'error' && !/status of 409 \(Conflict\)/.test(m.text())) c.errors.push(m.text()); });
        t.on('pageerror', (e) => c.errors.push(String(e)));
      }
      const t = c.sendTab;
      await open(t, `${c.serve.url}/#/s/${c.thread}`, ['/api/sessions']);
      const caught = new Promise<Route>((resolve) => {
        void t.route((u) => u.pathname === `/api/sessions/${c.thread}/prompt`, (r) => resolve(r));
      });
      await t.locator('#composer').fill('are you still there?');
      await t.getByRole('button', { name: 'Send', exact: true }).click();
      c.sendRoute = await caught;
      c.send = 'sent';
    },

    async SendAnswered(c) {
      const t = c.sendTab!;
      const at = Date.now();
      const res = t.waitForResponse((r) => new URL(r.url()).pathname === `/api/sessions/${c.thread}/prompt`);
      await c.sendRoute!.continue();
      c.sendRoute = undefined;
      const r = await res;
      if (r.status() >= 400 && r.status() < 500) {
        // Refused, and said beside the composer.
        await expect(t.locator('.send-failed-text').filter({ hasText: /project was deleted/ })).toBeVisible();
        c.send = 'refused';
        return;
      }
      if (!r.ok()) throw new Error(`send: ${r.status()} ${await r.text()}`);
      c.send = 'accepted';
      const deadline = Date.now() + 5_000;
      while (Date.now() < deadline) {
        try {
          const st = JSON.parse(fs.readFileSync(path.join(c.serve.home, '.bough', 'orbs', c.thread, 'state.json'), 'utf8')) as { status?: string; updatedAt?: string };
          if (st.status === 'failed' && Date.parse(st.updatedAt ?? '') > at) { c.send = 'orbfailed'; break; }
        } catch { /* not written yet */ }
        await sleep(50);
      }
    },

    async ByRepoRead(c) {
      await open(c.repoTab, `${c.serve.url}/#/projects`, ['/api/projects', '/api/sessions', '/api/projects/by-repo']);
      await expect(c.repoTab.getByText('Reading repos…')).toHaveCount(0);
    },

    // 'Create project' on the group the repo tab lists, named like gen 1
    // so it lands on the slug. The page files each session on its own and
    // says how many did not move.
    async ProjectFromRepo(c) {
      const t = c.repoTab;
      const g = repoGroup(c);
      const listed = await g.locator('.proj-row').count();
      await g.getByRole('button', { name: 'Create project', exact: true }).click();
      const d = t.getByRole('dialog');
      await d.getByRole('textbox').fill(c.name);
      await d.getByRole('button', { name: 'Create and move', exact: true }).click();
      const bar = t.locator('.rp-err');
      await expect(bar.or(d).first()).toBeVisible();
      await expect(d.locator('.dlg-err').or(bar).first()).toBeVisible();
      const not = Number(/(\d+) not moved/.exec((await bar.textContent().catch(() => '')) ?? '')?.[1] ?? 0);
      if (not > 0) {
        // Refused, and the page says why: "did not move" alone left the
        // person retrying a move serve will never make.
        await expect(d.locator('.dlg-err')).toContainText(/project session/);
      }
      if (await d.isVisible()) await d.getByRole('button', { name: 'Cancel', exact: true }).click();
      c.listed = listed; c.moved = listed - not; c.refused = not;
      c.gen = 2;
      await t.goto('about:blank');
      await c.pageTab.goto('about:blank');
    },

    // New project… with gen 1's name, once it is gone.
    async CreateProject(c) {
      const a = c.actor;
      await open(a, `${c.serve.url}/#/projects`, ['/api/projects', '/api/sessions']);
      await a.getByRole('button', { name: 'New project…' }).first().click();
      const d = a.getByRole('dialog');
      await d.getByRole('textbox').fill(c.name);
      await d.getByRole('button', { name: 'Create', exact: true }).click();
      await expect(d).toBeHidden();
      await expect(a.locator(`section.proj:has(a[href="#/projects/${c.slug}"])`)).toHaveCount(1);
      c.gen = 2;
      await c.repoTab.goto('about:blank');
      await c.pageTab.goto('about:blank');
    },
  },

  read: readUiState,
  // The walk's tab is the control room's sidebar.
  status: (c) => c.page.locator('.sidebar'),
  sessions: (c) => [c.main, c.thread].filter(Boolean),
  async check(c, where) {
    expect(c.errors, `${where}: console errors in the other tabs`).toEqual([]);
  },

  async cleanup(c) {
    if (c.held) release(c.dir, c.held);
    for (const f of fs.existsSync(c.dir) ? fs.readdirSync(c.dir) : []) if (f.startsWith(c.tag + '-') && f.endsWith('.json')) fs.rmSync(path.join(c.dir, f), { force: true });
    await c.sendRoute?.abort().catch(() => undefined);
    for (const t of [c.probe, c.actor, c.pageTab, c.repoTab, c.sendTab]) await t?.close().catch(() => undefined);
    for (const id of [c.thread, c.main]) if (id) await c.serve.api.post(`/api/sessions/${id}/archive`, { data: {} }).catch(() => undefined);
  },
});
