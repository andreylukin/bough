// go/tests/model/specs/context_inspector.fizz walked in the browser
// (recipe: go/tests/model/README.md): one session's Context page opened
// from the palette, its read answered or failed and retried, a file row
// opened and closed, the skill switched off and on, the page closed, and
// the session filed under a project and out of it. Every generated path
// is one test against a real serve; at each node readUiState must equal
// the spec's Context#0 state.
//
// The page decides nothing about when its read answers, so the walk
// does: every GET /api/sessions/{id}/context is held at the network
// until the spec's Resolve or Reject lets it go. That is what makes
// "loading" and a retry in flight states the page sits in rather than
// blinks.
//
// Per-test serve, not the worker's: helpers/model.ts builds on the
// `serve` fixture and a flow does not edit it (README). Nothing here
// needs more than one boot per path.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { expect, type Serve } from '../../helpers/serve';

// The fixture the spec's flags are about (AGENTS_LINES / CLAUDE_LINES):
// one file in the long band, one past it.
const AGENTS_LINES = 250;
const CLAUDE_LINES = 450;
const SKILL = 'ctxdemo';
const PROJECT = 'ctxproj';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  held: Route[];      // GET /api/sessions/{id}/context not yet answered
  homes: string[];    // HOME as serve spells it and as its realpath
  // skill_off is shown only on the loaded page; with it closed, loading
  // or failed the page shows no skills at all, so the last value it
  // showed is the one it would show again (off.yml is the only writer,
  // and only this page's button writes it here).
  skillOff: boolean;
}

// n distinct lines, so context-md's section dedup drops nothing.
const lines = (word: string, n: number) => Array.from({ length: n }, (_, i) => `${word} line ${i}\n`).join('');

// A read the page cannot tell from any other failure. A 500 does the
// same to the page, but Chromium logs every non-2xx fetch as a console
// error, which the per-node invariant forbids; a 200 whose body is not
// JSON fails contextApi.get the same way without that noise.
const failWith = (r: Route) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json: context read failed' });

// The oldest held read. It went out on the click that mounted the page
// (Open) or asked again (Retry), so it is there or about to be.
async function next(c: Ctx): Promise<Route> {
  const deadline = Date.now() + 10_000;
  while (c.held.length === 0) {
    if (Date.now() > deadline) throw new Error('context-inspector: no context read to answer');
    await new Promise((r) => setTimeout(r, 25));
  }
  return c.held.shift()!;
}

const pageHead = (c: Ctx) => c.page.locator('header.page-head').filter({ has: c.page.locator('h1', { hasText: /^Context$/ }) });
const fileRow = (c: Ctx, name: string) =>
  c.page.locator('details.ctx-file').filter({ has: c.page.locator(`.hk-path`, { hasText: new RegExp(`/${name.replace('.', '\\.')}$`) }) });
const skillRow = (c: Ctx) => c.page.locator('.hk2-row').filter({ has: c.page.locator('.hk2-name', { hasText: new RegExp(`^/${SKILL}$`) }) });

// A listed path as the spec names it: the cwd's files by basename, the
// project's MEMORY.md by name, home files as ~/….
function specName(c: Ctx, cwd: string, p: string): string {
  if (path.dirname(p) === cwd) return path.basename(p);
  for (const h of c.homes) {
    if (p.startsWith(path.join(h, '.bough', 'projects') + '/') && path.basename(p) === 'MEMORY.md') return 'MEMORY.md';
    if (p.startsWith(h + '/')) return '~/' + p.slice(h.length + 1);
  }
  return p;
}

const band = (cls: string | null) => (cls?.includes('ctx-lines-max') ? 'max' : cls?.includes('ctx-lines-long') ? 'long' : '');

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  let state = 'closed';
  let inflight = false;
  const onPage = (await pageHead(c).count()) > 0;
  if (onPage) {
    const retry = page.locator('.proj-body > p.err').getByRole('button', { name: /^Retry/ });
    if (await page.locator('.ctx-inv').count()) state = 'loaded';
    else if (await page.locator('.proj-body > p.err', { hasText: /^Context did not load:/ }).count()) {
      state = 'failed';
      inflight = (await retry.getAttribute('aria-busy')) === 'true';
    } else {
      state = 'loading';
      inflight = true;
    }
  }

  let files: string[] = [];
  let agents = '';
  let claude = '';
  let fileOpen = false;
  if (state === 'loaded') {
    const cwd = (await page.locator('.head-repo').textContent()) ?? '';
    // Read rows first, then the folded "not found" list: the page's order.
    const paths = await page.locator('details.ctx-file > .hk-path, details.ctx-missing > .hk-path').allTextContents();
    files = paths.map((p) => specName(c, cwd, p));
    agents = band(await fileRow(c, 'AGENTS.md').locator('.hk-when > span').first().getAttribute('class'));
    claude = band(await fileRow(c, 'CLAUDE.md').locator('.hk-when > span').first().getAttribute('class'));
    fileOpen = (await page.locator('details.ctx-file[open]').count()) > 0;
    const btn = (await skillRow(c).locator('.hk-offbtn').textContent()) ?? '';
    c.skillOff = btn.startsWith('Enable');
  }

  // The sidebar files a session under its project's group, whose head
  // names the project; a local session sits under its folder's.
  const head = page.locator('.ws').filter({ has: page.locator(`button.row[data-id="${c.id}"]`) }).locator('> .ws-head');
  const project = (await head.getAttribute('data-project').catch(() => null)) === PROJECT;

  return {
    agents_flag: agents,
    claude_flag: claude,
    file_open: fileOpen,
    files,
    inflight,
    page: state,
    project,
    skill_off: c.skillOff,
  };
}

// The Project picker in the session's settings: file it under PROJECT,
// or take it out.
async function assign(c: Ctx): Promise<void> {
  const { page } = c;
  const head = page.locator('.ws').filter({ has: page.locator(`button.row[data-id="${c.id}"]`) }).locator('> .ws-head');
  const inProject = (await head.getAttribute('data-project')) === PROJECT;
  const settings = page.getByRole('button', { name: 'Session settings' });
  await settings.click();
  await page.getByRole('combobox', { name: /^Project:/ }).click();
  await page.getByRole('option', { name: inProject ? 'Unassigned' : PROJECT, exact: true }).click();
  // The settings panel stays open after a choice; its button shuts it,
  // so the next Assign starts from the same closed panel.
  await expect(settings).toHaveAttribute('aria-expanded', 'true');
  await settings.click();
  await expect(settings).toHaveAttribute('aria-expanded', 'false');
}

modelTests<Ctx>({
  spec: 'context_inspector',
  role: 'Context#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    fs.writeFileSync(path.join(serve.work, 'AGENTS.md'), lines('agents', AGENTS_LINES));
    fs.writeFileSync(path.join(serve.work, 'CLAUDE.md'), lines('claude', CLAUDE_LINES));
    const skill = path.join(serve.home, '.claude', 'skills', SKILL, 'SKILL.md');
    fs.mkdirSync(path.dirname(skill), { recursive: true });
    fs.writeFileSync(skill, `---\nname: ${SKILL}\ndescription: a skill the context page can switch off\n---\nDemo.\n`);
    const res = await serve.api.post('/api/projects', { data: { name: PROJECT } });
    if (!res.ok()) throw new Error(`create project: ${res.status()} ${await res.text()}`);
    // The project's standing brief. The page lists files it read before
    // the ones it did not find, so a MEMORY.md that is not there would
    // be listed after AGENTS.md, and the spec's order is context-md's.
    fs.writeFileSync(path.join(serve.home, '.bough', 'projects', PROJECT, 'MEMORY.md'), 'Remember the fixture.\n');

    const c: Ctx = {
      page, serve, id: await serve.newSession(), held: [], skillOff: false,
      homes: [...new Set([serve.home, fs.realpathSync(serve.home)])],
    };
    await page.route((u) => u.pathname === `/api/sessions/${c.id}/context`, (r) => { c.held.push(r); });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await page.locator('#composer').waitFor();
    return c;
  },

  actions: {
    // The palette's "Inspect context": a session that has run no turn
    // has no Context chip in its strip to click.
    async Open(c) {
      await c.page.locator('#composer').click();
      await c.page.keyboard.press('ControlOrMeta+k');
      // The palette reopens on its last query; typing onto it made
      // "Inspect contextInspect context", which only New session matched.
      await c.page.keyboard.press('ControlOrMeta+a');
      await c.page.keyboard.type('Inspect context');
      await c.page.getByRole('option', { name: /^Inspect context/ }).click();
    },
    Resolve: async (c) => (await next(c)).continue(),
    Reject: async (c) => failWith(await next(c)),
    Retry: (c) => c.page.locator('.proj-body > p.err').getByRole('button', { name: /^Retry/ }).click(),
    ToggleSkill: (c) => skillRow(c).locator('.hk-offbtn').click(),
    OpenFile: (c) => fileRow(c, 'AGENTS.md').locator('summary').click(),
    CloseFile: (c) => c.page.locator('details.ctx-file[open] > summary').first().click(),
    // Esc unmounts the page (the header's Back button is the phone
    // layout's, hidden at this width); a read still held lands on
    // nothing, and is let go so the next Open's read is the next held.
    async Close(c) {
      await c.page.keyboard.press('Escape');
      for (const r of c.held.splice(0)) await r.continue().catch(() => {});
    },
    Assign: assign,
  },

  read: readUiState,
  // The header's title: the session's own on the transcript, "Context"
  // on the page.
  status: (c) => c.page.locator('header.thread-head h1').first(),
  sessions: (c) => [c.id],
  async cleanup(c) {
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
