// go/tests/model/specs/title_rename.fizz in the browser (recipe:
// go/tests/model/README.md): a session's name as the open thread shows it
// in the sidebar row, the thread's <h1> and the tab, and the Rename
// session dialog's success, failure and dismissal. Every generated path
// is walked through the page against a real serve whose model is
// llm-control.
import * as fs from 'fs';
import * as path from 'path';
import { test, type Page, type Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { loadPaths, modelTests, type Step } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const SPEC = 'title_rename';
const ROLE = 'Session#0';

// The spec's values are abstract; on the wire they are these texts, as in
// the Go adapter (tests/model/mbt/title_rename_test.go): the prompt is
// literally "prompt", and llm-control answers the session-title plugin's
// small-model call with "control".
const PROMPT = 'prompt';
const AUTO = 'control';

// What the page may know of the session's row: the fields its name is
// drawn from, as of the last list read the spec calls a Poll (or the
// refresh a successful rename awaits).
interface Known { title: string; summary: string }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  trace: Step[]; // the path this test walks, to read Rename's choice off
  step: number;
  held: string;  // the first turn, held until AutoTitle
  known: Known;
  prior: string; // meta when the dialog opened: the client's own record, as `viewing` is in the example
  // RenameFail's 500s not yet logged: Chromium logs each failed POST once,
  // and the log can land after the step's own wait.
  failures: number;
}

const abstract = (s: string) => (s === AUTO ? 'auto' : s);

// The page's name for a session with none is "Untitled · <date>" (the id
// tail is a separate chip, not the name): the spec's "".
const shownName = (s: string) => abstract(/^Untitled( · |\s+session$)/.test(s) ? '' : s.trim());

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const dialog = (c: Ctx) => c.page.getByRole('dialog', { name: 'Rename session' });

// --- the server's side: hist, meta and title are not on the page until
// it reads them, so they are read off serve the way the Go adapter does
// (history's transcript, meta.json on disk, the list row). The page's
// fields below come from the DOM only.

function historyTitle(c: Ctx): string {
  const file = path.join(c.serve.home, '.bough', 'history', c.id + '.jsonl');
  if (!fs.existsSync(file)) return '';
  let title = '';
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    if (!line.trim()) continue;
    const e = JSON.parse(line) as { kind: string; data?: Record<string, unknown> };
    const d = e.data ?? {};
    if (e.kind === 'title' && typeof d.text === 'string' && d.text) { title = d.text; continue; }
    if (e.kind === 'input' && !title) title = String(d.typed || d.text || '').split('\n')[0];
  }
  return title;
}

const metaPath = (c: Ctx) => path.join(c.serve.home, '.bough', 'serve', 'meta.json');

function metaTitle(c: Ctx): string {
  if (!fs.existsSync(metaPath(c))) return '';
  const f = JSON.parse(fs.readFileSync(metaPath(c), 'utf8')) as { sessions?: Record<string, { title?: string }> };
  return f.sessions?.[c.id]?.title ?? '';
}

async function serverRow(c: Ctx): Promise<Known> {
  const res = await c.serve.api.get('/api/sessions');
  if (!res.ok()) throw new Error(`list: ${res.status()} ${await res.text()}`);
  const r = ((await res.json()).sessions as { id: string; title?: string; summary?: string }[]).find((x) => x.id === c.id);
  if (!r) throw new Error(`session ${c.id} not in the list`);
  return { title: r.title ?? '', summary: r.summary ?? '' };
}

// --- the page's reads of the row. The page takes a row from the list
// poll and, for the open session, from every transcript catch-up the
// event stream sets off; the page's clock is moved before every state
// read, so either could land at any node. The spec's Poll is the one
// moment the page learns the name, so the session's row is served to the
// page with the name it had at the last Poll (or successful rename): what
// is asserted is that the page then shows that name, the same on every
// surface, and that a rename shows the new one before its dialog closes.

type Body = { session?: { id: string } & Partial<Known>; sessions?: ({ id: string } & Partial<Known>)[] };

async function asKnown(c: Ctx, route: Route): Promise<void> {
  const res = await route.fetch().catch(() => null);
  // A read still out when the test's serve stops.
  if (!res) return route.abort().catch(() => {});
  const type = res.headers()['content-type'] ?? '';
  if (!res.ok() || !type.includes('json')) return route.fulfill({ response: res });
  const body = (await res.json()) as Body;
  const fix = (r?: { id: string } & Partial<Known>) => { if (r?.id === c.id) Object.assign(r, c.known); };
  fix(body.session);
  body.sessions?.forEach(fix);
  return route.fulfill({ response: res, json: body });
}

async function routes(c: Ctx): Promise<void> {
  await c.page.route((u) => u.pathname === '/api/sessions' || u.pathname === `/api/sessions/${c.id}`, (route) =>
    route.request().method() === 'GET' ? asKnown(c, route) : route.continue());
  // A rename that serve took is what the page's awaited refresh shows.
  await c.page.route((u) => u.pathname === `/api/sessions/${c.id}/rename`, async (route) => {
    const res = await route.fetch();
    if (res.ok()) c.known = await serverRow(c);
    return route.fulfill({ response: res });
  });
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const label = (await row(c).getAttribute('aria-label')) ?? '';
  const header = (await c.page.locator('.thread-head h1').textContent()) ?? '';
  const tab = (await c.page.title()).replace(/ · bough$/, '');
  return {
    hist: abstract(historyTitle(c)),
    meta: abstract(metaTitle(c)),
    title: abstract((await serverRow(c)).title),
    sidebar: shownName(label.split(', ')[0]),
    header: shownName(header),
    tab: shownName(tab),
    dialog: await dialog(c).isVisible(),
    failed: await c.page.locator('#dlg-err').isVisible(),
    prior: abstract(c.prior),
  };
}

// The Rename choice ("" or "mine") is the spec's; the harness passes an
// action no step, so it is read off the path this test walks: the name
// meta holds after the step.
function renameText(c: Ctx): string {
  return String(c.trace[c.step].state[`${ROLE}.meta`]);
}

async function submit(c: Ctx, text: string): Promise<void> {
  const d = dialog(c);
  await d.getByRole('textbox').fill(text);
  await d.getByRole('button', { name: 'Rename', exact: true }).click();
}

let turns = 0;

modelTests<Ctx>({
  spec: SPEC,
  role: ROLE,
  config: CONTROL_CONFIG,
  shared: true,
  reset: true,

  async init(page, serve) {
    const n = Number(/^path (\d+):/.exec(test.info().title)?.[1]);
    const c: Ctx = {
      page, serve, id: await serve.newSession(), trace: loadPaths(SPEC)[n], step: 0,
      held: '', known: { title: '', summary: '' }, prior: '', failures: 0,
    };
    await routes(c);
    await page.goto(`${serve.url}/#/s/${c.id}`);
    return c;
  },

  actions: {
    // The first prompt, its turn held so history's title stays the
    // opening line until AutoTitle. Through the open thread's composer;
    // while the Rename dialog is up the page behind it is inert, so the
    // same POST comes from another client (the spec lets a prompt land
    // then: the dialog is not the only way in).
    async Prompt(c) {
      c.step++;
      const name = `r${process.pid}-${String(++turns).padStart(4, '0')}`;
      const dir = controlDir(c.serve.home);
      queue(dir, name, { mode: 'block', text: `finished ${name}` });
      if (await dialog(c).isVisible()) {
        const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: PROMPT } });
        if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
      } else {
        await c.page.locator('#composer').fill(PROMPT);
        await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      }
      c.held = name;
      await waitTaken(dir, name);
    },
    // The first turn ends; the session-title plugin writes its one name.
    async AutoTitle(c) {
      c.step++;
      release(controlDir(c.serve.home), c.held);
      c.held = '';
    },
    // The list poll: 12 s with a session open, on the page's own timer.
    async Poll(c) {
      c.step++;
      c.known = await serverRow(c);
      const read = c.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/sessions');
      await c.page.clock.fastForward(12_000);
      await read;
    },
    async OpenRename(c) {
      c.step++;
      c.prior = metaTitle(c);
      await c.page.getByRole('button', { name: 'Session settings' }).click();
      await c.page.getByRole('button', { name: 'Rename…' }).click();
      await dialog(c).waitFor();
    },
    async Rename(c) {
      c.step++;
      await submit(c, renameText(c));
      await dialog(c).waitFor({ state: 'hidden' });
      c.prior = '';
    },
    // serve's meta save fails (a directory where it writes meta.json.tmp,
    // as the Go adapter does) for a rename that would change the name.
    async RenameFail(c) {
      c.step++;
      const tmp = metaPath(c) + '.tmp';
      fs.mkdirSync(tmp, { recursive: true });
      try {
        const header = shownName((await c.page.locator('.thread-head h1').textContent()) ?? '');
        // The answer, not the error line: whether the dialog then shows
        // it and stays up is the state read's to say.
        const posted = c.page.waitForResponse((r) => new URL(r.url()).pathname === `/api/sessions/${c.id}/rename`);
        c.failures++; // before the POST: the log can come before the answer does
        await submit(c, header === 'mine' ? '' : 'mine');
        if ((await posted).ok()) throw new Error('RenameFail: POST rename succeeded with meta.json.tmp blocked');
      } finally {
        // Gone already once the test's serve stopped and took HOME with it.
        if (fs.existsSync(tmp)) fs.rmdirSync(tmp);
      }
    },
    async Dismiss(c) {
      c.step++;
      await dialog(c).getByRole('button', { name: 'Cancel', exact: true }).click();
      await dialog(c).waitFor({ state: 'hidden' });
      c.prior = '';
    },
  },

  // Chromium logs the failed POST itself; the dialog saying so is what
  // the step checks.
  expectedError(c, text) {
    if (c.failures === 0 || !/^Failed to load resource: .* status of 500\b/.test(text)) return false;
    c.failures--;
    return true;
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.held) release(controlDir(c.serve.home), c.held);
  },
});
