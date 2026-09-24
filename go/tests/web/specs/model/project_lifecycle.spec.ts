// go/tests/model/specs/project_lifecycle.fizz in the browser: one project
// slot ("alpha") created, filed into, renamed, broken and fixed, given a
// MEMORY.md and deleted from the Projects page, with an "agent" writing
// ~/.bough/projects/alpha behind the page's back. Recipe:
// go/tests/model/README.md.
//
// Two tabs read the state. The walk's tab is the person who has been
// looking at the Projects page all along: `shown` is its project list.
// Everything else is what the directory holds, and that is read off a
// second tab reloaded at every read — anyone opening the control room
// now — so a lag in the first tab is a lag, not a wrong answer.
//
// One serve per worker: 53 short paths, and a boot per path was most of
// the run. Init empties the slot the way the page does (Delete), which
// also unfiles the one conversation and takes MEMORY.md with it.
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { expect, type Serve } from '../../helpers/serve';

const SLUG = 'alpha';
// A definition that touches nothing on the host: no repos to check, no identity.
const VALID_YAML = 'name: Alpha\nrepos: []\n';
// Broken past the name: line, so a rename is refused rather than repairing it.
const BROKEN_YAML = 'name: Alpha\nrepos: [\n';

interface Ctx {
  page: Page;
  probe: Page;
  serve: Serve;
  convo: string;
}

// The one conversation, made once per serve: its turn names ~/repos/widget
// so 'Create project from widget' has a group to offer.
const convos = new Map<string, Promise<string>>();
function conversation(serve: Serve): Promise<string> {
  let p = convos.get(serve.url);
  if (!p) {
    p = (async () => {
      queue(controlDir(serve.home), 'p0000', { mode: 'ok', text: 'looked' });
      const id = await serve.newSession('look at ~/repos/widget');
      await expect.poll(async () => {
        const res = await serve.api.get('/api/sessions');
        const { sessions } = (await res.json()) as { sessions: { id: string; status: string }[] };
        return sessions.find((r) => r.id === id)?.status;
      }, { message: 'the widget turn to finish', timeout: 20_000 }).toBe('done');
      return id;
    })();
    convos.set(serve.url, p);
  }
  return p;
}

const yamlPath = (c: Ctx) => path.join(c.serve.home, '.bough', 'projects', SLUG, 'project.yml');

// A project's section on a Projects page, found by its Open link: the slug
// is the key, the heading is only its name.
const section = (p: Page) => p.locator(`section.proj:has(a[href="#/projects/${SLUG}"])`);

// The conversation rows of a section; the orb's container rows share the class.
const convoRows = (p: Page) => section(p).locator('.proj-row:not(.orb-row)');

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  // The walk's tab: only what its list last read.
  const mine = section(c.page);
  const shown = (await mine.count()) === 0 ? 'none' : (await mine.locator('.proj-bad').count()) > 0 ? 'broken' : 'ok';

  // A fresh tab: the directory as it is now. Its orb section is opened by
  // the route, for MEMORY.md.
  const read = (u: string) => (r: { url(): string }) => new URL(r.url()).pathname === u;
  const lists = Promise.all([c.probe.waitForResponse(read('/api/projects')), c.probe.waitForResponse(read('/api/sessions'))]);
  await c.probe.reload();
  await lists;
  // The conversation is always listed somewhere: once it is, the rows
  // have rendered, and the project list landed with them.
  await expect(c.probe.locator('.proj-row:not(.orb-row)')).toHaveCount(1);
  await c.probe.waitForTimeout(150);
  const sec = section(c.probe);
  if ((await sec.count()) === 0) return { disk: 'none', name: '', slug: '', shown, memory: '', convo: 'loose' };
  const href = (await sec.getByRole('link', { name: 'Open' }).getAttribute('href')) ?? '';
  const orb = sec.locator('.proj-orb');
  await orb.getByRole('tab', { name: 'MEMORY.md' }).click();
  const memory = await orb.getByRole('textbox', { name: 'MEMORY.md' }).inputValue();
  return {
    disk: (await sec.locator('.proj-bad').count()) > 0 ? 'broken' : 'ok',
    name: (await sec.locator('h2').innerText()).trim(),
    slug: href.replace('#/projects/', ''),
    shown,
    memory: memory ? 'saved' : '',
    convo: (await convoRows(c.probe).count()) > 0 ? 'filed' : 'loose',
  };
}

const dialog = (c: Ctx) => c.page.getByRole('dialog');

// Types into the open dialog and presses its action. A refusal must say
// why inside the dialog; the person then cancels it.
async function answer(c: Ctx, text: string, action: string, refused?: RegExp): Promise<void> {
  const d = dialog(c);
  await expect(d).toBeVisible();
  await d.getByRole('textbox').fill(text);
  await d.getByRole('button', { name: action, exact: true }).click();
  if (refused) {
    await expect(d.locator('.dlg-err')).toHaveText(refused);
    await d.getByRole('button', { name: 'Cancel', exact: true }).click();
  }
  await expect(d).toBeHidden();
}

async function menu(c: Ctx, item: string): Promise<void> {
  await section(c.page).getByRole('button', { name: /^More actions for / }).click();
  await c.page.getByRole('menuitem', { name: item, exact: true }).click();
}

// The project's orb section on the walk's tab, opened if it is not.
async function orb(c: Ctx, file: 'MEMORY.md' | 'project.yml') {
  const btn = section(c.page).getByRole('button', { name: 'Orb', exact: true });
  if ((await btn.getAttribute('aria-expanded')) !== 'true') await btn.click();
  const o = section(c.page).locator('.proj-orb');
  await o.getByRole('tab', { name: file }).click();
  return o;
}

async function save(c: Ctx, file: 'MEMORY.md' | 'project.yml', text: string, refused = false): Promise<void> {
  const o = await orb(c, file);
  await o.getByRole('textbox', { name: file }).fill(text);
  await o.getByRole('button', { name: 'Save', exact: true }).click();
  if (refused) await expect(o.locator('.orb-save-err')).toBeVisible();
  else {
    // Saved: the draft is gone, so there is nothing left to save.
    await expect(o.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
    await expect(o.locator('.orb-save-err')).toHaveCount(0);
  }
}

const newProject = (c: Ctx) => c.page.getByRole('button', { name: 'New project…' }).first().click();

modelTests<Ctx>({
  spec: 'project_lifecycle',
  role: 'Project#0',
  config: CONTROL_CONFIG,
  // The list asks the runtime whether each project's image exists; this
  // flow has no containers, so the host's engine must not be the one asked.
  env: { BOUGH_CONTAINER: 'none' },
  shared: true,
  // Refresh (the 30 s projects poll) is an action of the spec: a read
  // that moved the clock would run it between steps and hide every place
  // the page forgets to re-read its list after a person acts.
  pollStepMs: 0,

  async init(page, serve) {
    const convo = await conversation(serve);
    const res = await serve.api.get('/api/projects');
    const { projects } = (await res.json()) as { projects: { slug: string }[] };
    if (projects.some((p) => p.slug === SLUG)) {
      const del = await serve.api.delete(`/api/projects/${SLUG}`);
      if (!del.ok()) throw new Error(`init: delete: ${del.status()} ${await del.text()}`);
    }
    await page.goto(`${serve.url}/#/projects`);
    const probe = await page.context().newPage();
    await probe.goto(`${serve.url}/#/projects/${SLUG}/orb`);
    return { page, probe, serve, convo };
  },

  actions: {
    async Create(c) {
      await newProject(c);
      await answer(c, 'Alpha', 'Create');
    },
    async CreateTaken(c) {
      await newProject(c);
      await answer(c, 'Alpha', 'Create', /already exists/);
    },
    async CreateBadName(c) {
      await newProject(c);
      await answer(c, '!!!', 'Create', /./);
    },
    async CreateFromRepo(c) {
      const group = c.page.locator('.rp-repo', { has: c.page.locator('.rp-repo-name', { hasText: /^widget$/ }) });
      await group.getByRole('button', { name: 'Create project', exact: true }).click();
      await answer(c, 'Alpha', 'Create and move');
    },
    async File(c) {
      await c.page.getByRole('combobox', { name: /^Move to project, now in Unassigned/ }).click();
      // Unassigned first, then the one project.
      await c.page.getByRole('listbox').getByRole('option').nth(1).click();
      await expect(convoRows(c.page)).toHaveCount(1);
    },
    async Rename(c) {
      const name = (await section(c.page).locator('h2').innerText()).trim();
      await menu(c, 'Rename…');
      await answer(c, name === 'Alpha' ? 'Beta' : 'Alpha', 'Rename');
    },
    async RenameBroken(c) {
      await menu(c, 'Rename…');
      await answer(c, 'Beta', 'Rename', /./);
    },
    async Delete(c) {
      await menu(c, 'Delete…');
      await answer(c, SLUG, 'Delete project');
    },
    async DeleteMistyped(c) {
      await menu(c, 'Delete…');
      await answer(c, 'alpah', 'Delete project', new RegExp(`type ${SLUG} to confirm`));
    },
    SaveMemory: (c) => save(c, 'MEMORY.md', '# Alpha\n\nremember the widget\n'),
    FixYaml: (c) => save(c, 'project.yml', VALID_YAML),
    // Parses, but names a checkout that is not there: the validation list.
    SaveYamlInvalid: (c) => save(c, 'project.yml', `name: Alpha\nrepos:\n  - path: ${path.join(c.serve.home, 'no-such-checkout')}\n`, true),
    // The agent's file tools: no request, and the page is not told.
    async AgentCreate(c) {
      fs.mkdirSync(path.dirname(yamlPath(c)), { recursive: true });
      fs.writeFileSync(yamlPath(c), VALID_YAML);
    },
    async AgentBreak(c) {
      fs.writeFileSync(yamlPath(c), BROKEN_YAML);
    },
    // The page's projects poll reads at most every 30 s; move its clock past that.
    Refresh: (c) => c.page.clock.fastForward(31_000),
  },

  read: readUiState,
  status: (c) => c.page.locator('.proj-page-head .head-repo'),
  sessions: (c) => [c.convo],
  async cleanup(c) {
    await c.probe.close();
  },
});
