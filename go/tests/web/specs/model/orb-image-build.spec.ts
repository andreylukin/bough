// go/tests/model/specs/orb-image-build.fizz in the browser: one
// project's image build as Projects → Orb drives it (recipe:
// go/tests/model/README.md). Every generated path is walked through the
// page, and at each node readUiState must equal the spec's Project role.
//
// The serve is tests/model/orbserve, not bough: the same API, guard and
// embedded page over a container.Fake that parks serve's build goroutine
// at the spec's steps (first image check, Commit) until this file
// answers over /fake/*. A real `bough serve` on darwin builds with the
// Apple CLI whatever PATH says.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { loadPaths, modelTests, type Step } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

const SPEC = 'orb-image-build';
const ROLE = 'Project#0';
const NAME = 'Orbit';

// One build per worker process; ORB_SERVE_BIN skips it (CI, the model
// script). Built in beforeAll, not at import: the runner's own process
// imports this file too, only to list the tests.
const orbServeBin = process.env.ORB_SERVE_BIN ?? path.join(os.tmpdir(), `bough-orbserve-${process.pid}`);
test.beforeAll(() => {
  if (process.env.ORB_SERVE_BIN) return;
  execFileSync('go', ['build', '-o', orbServeBin, './tests/model/orbserve'], {
    cwd: path.resolve(__dirname, '..', '..', '..', '..'), stdio: 'inherit',
  });
});
test.use({ serveOpts: { bin: orbServeBin } });
// The longest paths are eight actions, each a server round trip and a
// polled read: under a loaded machine 30 s was not always enough.
test.describe.configure({ timeout: 60_000 });

interface Ctx {
  page: Page;
  serve: Serve;
  slug: string;
  yml: string;     // the project.yml it was created with, to restore after a broken edit
  trace: Step[];   // this test's path: Edit's `oneof ok` is the next state's valid
  step: number;
  edits: number;
}

const panel = (c: Ctx) => c.page.locator('.proj-orb');
const verdict = (c: Ctx) => panel(c).locator('.orb-verdict');
const buildButton = (c: Ctx) => panel(c).getByRole('button', { name: /^(Build image|Building…)$/ });
const dialog = (c: Ctx) => c.page.getByRole('dialog', { name: `${NAME}’s image build failed` });
const orbRoute = (c: Ctx) => `#/projects/${c.slug}/orb`;

// The fake runtime's lines (orbserve's Commit, container.Fake's Commit,
// EnsureImage's own failure line): what one build's log holds.
const START = 'fake: building ';
const OK = 'fake commit ';
const FAILED = 'build failed:';

// build.json as the page shows it. The log is one build's: EnsureImage
// truncates it when it writes build.json building, and the page must
// show exactly that build's lines — a second start line means the page
// appended a new build's log to the last one's.
function jsonOf(log: string): string {
  if (log === '') return '';
  const starts = log.split('\n').filter((l) => l.startsWith(START)).length;
  if (starts !== 1) return `unknown log: ${JSON.stringify(log)}`;
  if (log.includes(FAILED)) return 'failed';
  if (log.includes(OK)) return 'ok';
  return 'building';
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = panel(c);
  const word = (await verdict(c).locator('.orb-fact-v').first().textContent()) ?? '';
  const running = (await buildButton(c).textContent()) === 'Building…';
  const hasLog = (await p.locator('.orb-log').count()) > 0;
  const shown = hasLog ? (await p.locator('.orb-log').textContent()) ?? '' : '';
  const log = shown === 'Nothing logged yet.' ? '' : shown;
  // The log summary's state word: build.json's, with serve's own failure
  // over it. The page shows one "failed" for both records, which is why
  // the spec's log_state merges them too.
  const said = ((await p.locator('.block-lines').count()) > 0 ? (await p.locator('.block-lines').textContent()) ?? '' : '')
    .replace(/^older image · /, '').replace(/^never built$/, '');
  const json = jsonOf(log);
  // A definition that does not load is the verdict's "Orb failed"; the
  // summary stops before it asks for an image, so nothing reads built.
  const valid = !word.includes('Orb failed');
  // "Ready", "Ready · last build failed", "Ready · rebuilding image": an
  // image for this definition exists.
  const out: Record<string, unknown> = {
    runs: running ? 1 : 0,
    phase: !running ? 'idle' : json === 'building' ? 'building' : 'syncing',
    json,
    built: valid && word.includes('Ready'),
    err: !running && said === 'failed',
    // How the last build ended, in the page's words; nothing while one runs.
    last: !running && (said === 'ok' || said === 'failed') ? said : '',
    valid,
    confirm: await dialog(c).isVisible(),
  };
  // The log went empty and came back the same: the page threw away what
  // it had and read it all again (see installLogWatch).
  const flash = await c.page.evaluate(() => (window as unknown as { __orbLog?: { flash: string } }).__orbLog?.flash ?? '');
  if (flash) out.logFlashedEmpty = flash;
  return out;
}

// Every text the build log <pre> shows, per element: a pre that goes
// X → empty → X re-read something it already had. A new element (the
// panel remounted) starts over, since an empty first paint is normal.
function installLogWatch(): void {
  const s = { el: null as Element | null, seen: [] as string[], flash: '' };
  (window as unknown as { __orbLog: typeof s }).__orbLog = s;
  new MutationObserver(() => {
    const el = document.querySelector('.proj-orb .orb-log');
    if (!el) return;
    const raw = el.textContent ?? '';
    const t = raw === 'Nothing logged yet.' ? '' : raw;
    if (el !== s.el) { s.el = el; s.seen = [t]; return; }
    if (s.seen[s.seen.length - 1] === t) return;
    s.seen.push(t);
    const n = s.seen.length;
    if (!s.flash && n >= 3 && t !== '' && s.seen[n - 2] === '' && s.seen[n - 3] === t) s.flash = t;
  }).observe(document, { subtree: true, childList: true, characterData: true });
}

async function fake(c: Ctx, method: 'GET' | 'POST', what: string): Promise<Record<string, unknown>> {
  const res = method === 'GET' ? await c.serve.api.get(`/fake/${what}`) : await c.serve.api.post(`/fake/${what}`);
  if (!res.ok()) throw new Error(`fake ${what}: ${res.status()} ${await res.text()}`);
  return res.json();
}

// Waits for serve, not the page: the page's own catching up is what the
// harness's read polls for.
async function settle(c: Ctx, done: (held: string, log: string) => boolean, what: string): Promise<void> {
  await expect.poll(async () => {
    const held = String((await fake(c, 'GET', 'held')).held);
    const log = String((await (await c.serve.api.get(`/api/projects/${c.slug}/orb/build/log?offset=0`)).json()).state);
    return done(held, log);
  }, { message: `${what}: serve's build did not settle`, timeout: 10_000 }).toBe(true);
}

async function release(c: Ctx, which: 'syncing' | 'building', err = ''): Promise<void> {
  const q = `release?which=${which}` + (err ? `&err=${encodeURIComponent(err)}` : '');
  expect((await fake(c, 'POST', q)).released, `nothing held at ${which}`).toBe(true);
}

// The spec's state before this action.
const before = (c: Ctx) => c.trace[c.step - 1].state;
const field = (st: Record<string, unknown>, f: string) => st[`${ROLE}.${f}`];

async function save(c: Ctx, file: string, text: string): Promise<void> {
  const p = panel(c);
  await p.getByRole('tab', { name: file }).click();
  await p.getByRole('textbox', { name: file, exact: true }).fill(text);
  const put = c.page.waitForResponse((r) => r.request().method() === 'PUT' && r.url().includes(`/orb/files/${file}`));
  await p.getByRole('button', { name: 'Save', exact: true }).click();
  expect((await put).ok(), `save ${file}`).toBe(true);
}

// The project list the page's New session reads (failedBuild) comes
// from its own poll, not the orb panel's detail: move the page's clock
// past one poll and let it land before clicking.
async function freshList(c: Ctx): Promise<void> {
  const listed = c.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/projects' && r.request().method() === 'GET');
  await c.page.clock.fastForward(5_000);
  await listed;
}

modelTests<Ctx>({
  spec: SPEC,
  role: ROLE,

  async init(page, serve) {
    const res = await serve.api.post('/api/projects', { data: { name: NAME } });
    if (!res.ok()) throw new Error(`create project: ${res.status()} ${await res.text()}`);
    const slug: string = (await res.json()).project.slug;
    const yml = fs.readFileSync(path.join(serve.home, '.bough', 'projects', slug, 'project.yml'), 'utf8');
    const i = Number(/^path (\d+):/.exec(test.info().title)?.[1]);
    const c: Ctx = { page, serve, slug, yml, trace: loadPaths(SPEC)[i], step: 0, edits: 0 };
    await page.addInitScript(installLogWatch);
    await page.goto(`${serve.url}/${orbRoute(c)}`);
    return c;
  },

  actions: {
    // The button while the page offers it; when it does not (a build
    // running, a definition that does not load) the same POST from
    // another client, which serve must refuse without changing anything.
    async Build(c) {
      c.step++;
      const st = before(c);
      const accepted = field(st, 'valid') === true && field(st, 'runs') === 0;
      if (await buildButton(c).isEnabled()) {
        if (accepted) await fake(c, 'POST', `arm?slug=${c.slug}`);
        await buildButton(c).click();
      } else {
        const res = await c.serve.api.post(`/api/projects/${c.slug}/orb/build`, { data: {} });
        expect(res.status(), 'Build refused').toBe(field(st, 'valid') === true ? 409 : 400);
      }
      if (accepted) await settle(c, (held) => held === 'syncing', 'Build');
    },
    async SyncFail(c) {
      c.step++;
      await release(c, 'syncing', 'fake: clone failed');
      await settle(c, (held, log) => held === 'idle' && log !== 'building', 'SyncFail');
    },
    // The image check answers; an image that exists and did not fail ends
    // the build there, anything else goes on to the parked Commit.
    async Begin(c) {
      c.step++;
      await release(c, 'syncing');
      await settle(c, (held, log) => held === 'building' || (held === 'idle' && log !== 'building'), 'Begin');
    },
    async BuildOk(c) {
      c.step++;
      await release(c, 'building');
      await settle(c, (held, log) => held === 'idle' && log !== 'building', 'BuildOk');
    },
    async BuildFail(c) {
      c.step++;
      await release(c, 'building', 'fake: setup.sh exited 1');
      await settle(c, (held, log) => held === 'idle' && log !== 'building', 'BuildFail');
    },
    // A good edit goes through the panel's editor: project.yml put back
    // if it was broken, and a setup.sh change, so the tag is new. A
    // broken one is written to disk the way an agent with the file tools
    // can: the editor refuses a project.yml that does not load.
    async Edit(c) {
      c.step++;
      const ok = field(c.trace[c.step].state, 'valid') === true;
      const file = path.join(c.serve.home, '.bough', 'projects', c.slug, 'project.yml');
      if (!ok) { fs.writeFileSync(file, 'repos: [\n'); return; }
      if (field(before(c), 'valid') !== true) await save(c, 'project.yml', c.yml);
      const setup = await panel(c).getByRole('tab', { name: 'setup.sh' }).click()
        .then(() => panel(c).getByRole('textbox', { name: 'setup.sh', exact: true }).inputValue());
      await save(c, 'setup.sh', `${setup}${setup && !setup.endsWith('\n') ? '\n' : ''}# edit ${++c.edits}\n`);
    },
    async StartSession(c) {
      c.step++;
      await freshList(c);
      await c.page.locator('section.proj').filter({ has: c.page.getByRole('heading', { name: NAME }) })
        .getByRole('button', { name: 'New session', exact: true }).click();
    },
    // Starts the session (orbserve's stand-in child) and the page opens
    // it on the project's page; the person then comes back to the orb panel.
    //
    // serve answers a project thread's POST with the id it minted before
    // the child has written its transcript, and the page opening it at
    // once got 404s (console errors) for it: the session-start flow's
    // race, not this one's. The page gets serve's answer unchanged, only
    // once serve knows the session.
    async StartAnyway(c) {
      c.step++;
      let created = '';
      await c.page.route('**/api/sessions', async (route) => {
        if (route.request().method() !== 'POST') return route.fallback();
        const res = await route.fetch();
        const body = await res.text();
        if (res.ok()) {
          const id: string = JSON.parse(body).session.id;
          await expect.poll(async () => (await c.serve.api.get(`/api/sessions/${id}`)).status(), { message: `StartAnyway: ${id} never listed` }).toBe(200);
          created = id;
        } else created = `${res.status()} ${body}`;
        await route.fulfill({ response: res, body });
      });
      await dialog(c).getByRole('button', { name: 'Start anyway' }).click();
      await expect.poll(() => created, { message: 'StartAnyway: no session created' }).not.toBe('');
      await c.page.unroute('**/api/sessions');
      expect(created, 'StartAnyway: session not created').not.toMatch(/^\d{3} /);
      await expect.poll(() => c.page.evaluate(() => location.hash), { message: 'StartAnyway: session not opened' }).not.toBe(orbRoute(c));
      await c.page.evaluate((h) => { location.hash = h; }, orbRoute(c));
    },
    async OpenOrb(c) {
      c.step++;
      await dialog(c).getByRole('button', { name: 'Open orb' }).click();
    },
  },

  read: readUiState,
  status: verdict,
  sessions: () => [],
  async cleanup(c) {
    // A parked build keeps serve's goroutine; stopping serve lets it go too.
    await release(c, 'syncing', 'walk over').catch(() => {});
    await release(c, 'building', 'walk over').catch(() => {});
  },
});
