// The browser walk of go/tests/model/specs/project_rename_vs_orb_identity.fizz:
// renaming a project never moves anything orb-shaped, and an open page's own
// display of the name only ever catches up on its own poll. Recipe:
// go/tests/model/README.md.
//
// ui_name is the one field this flow is actually about: an open Projects
// page's own cached render of the name, read from the DOM. The page that
// does the renaming calls its own immediate refresh() right after
// (app.tsx's onRename), so ui_name is read off a second, passive tab that
// never acts — Rename is driven through a `probe` tab instead, the same
// split project_lifecycle.spec.ts uses for "shown" (a tab's own cache) vs.
// "disk" (a fresh read), just with the roles swapped: here `page` is the
// stale cache and `probe` does the writing. `page`'s cache only moves on
// its own 30 s poll (pollStepMs: 0, UiPoll fast-forwards past it).
//
// y_name (what's really on disk) and orb_building/orb_running/orb_identity
// have no pixels anywhere in the UI — this flow's product surface
// (supervisor.RenameProject, and the image tag a build would use right
// now) is entirely backend, never rendered — so they are read straight off
// serve's own API, the way go/tests/model/mbt's adapter reads them off the
// supervisor in process. orb_building/orb_running are never driven through
// serve at all (no build to race, as the mbt adapter's comment says);
// StartOrbBuild/FinishOrbBuild/StopOrb only flip this test's own bookkeeping.
import type { Page } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import { expect, type Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;  // passive: never renames, only ever read
  probe: Page; // does the renaming, through the same Projects UI
  serve: Serve;
  slug: string;
  alphaName: string;
  betaName: string;
  building: boolean;
  running: boolean;
  // The tag Init captured, before any rename: identity() below classifies
  // against it, so only a real drift ever prints as something other than
  // the spec's literal.
  baseline: string;
}

// The project's section on the Projects page, found by its Open link (the
// slug is the key, the heading is only its name) — same locator as
// project_lifecycle.spec.ts.
const section = (p: Page, slug: string) => p.locator(`section.proj:has(a[href="#/projects/${slug}"])`);

function classify(c: Ctx, name: string): string {
  if (name === c.alphaName) return 'Alpha';
  if (name === c.betaName) return 'Beta';
  return `other:${name}`;
}

async function diskName(c: Ctx): Promise<string> {
  const res = await c.serve.api.get(`/api/projects/${c.slug}`);
  if (!res.ok()) throw new Error(`GET project: ${res.status()} ${await res.text()}`);
  const { name } = (await res.json()) as { name: string };
  return name;
}

// The tag EnsureImage would use for a build started right now: the real
// slug-keyed hash, off the same orb detail endpoint the orb page reads.
async function identity(c: Ctx): Promise<string> {
  const res = await c.serve.api.get(`/api/projects/${c.slug}/orb`);
  if (!res.ok()) throw new Error(`GET orb: ${res.status()} ${await res.text()}`);
  const { orb } = (await res.json()) as { orb: { image: string } };
  return orb.image === c.baseline ? 'alpha-slug' : orb.image;
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const sec = section(c.page, c.slug);
  const uiName = (await sec.count()) === 0 ? '' : (await sec.locator('h2').innerText()).trim();
  return {
    y_name: classify(c, await diskName(c)),
    ui_name: classify(c, uiName),
    orb_building: c.building,
    orb_running: c.running,
    orb_identity: await identity(c),
  };
}

modelTests<Ctx>({
  spec: 'project_rename_vs_orb_identity',
  role: 'Project#0',
  // This flow has no containers and no LLM turns; the host's real engine
  // must not be the one an orb-detail read asks (project_lifecycle.spec.ts).
  env: { BOUGH_CONTAINER: 'none' },

  async init(page, serve) {
    const res = await serve.api.post('/api/projects', { data: { name: 'Alpha' } });
    if (!res.ok()) throw new Error(`create project: ${res.status()} ${await res.text()}`);
    const { project } = (await res.json()) as { project: { slug: string } };
    const slug = project.slug;
    const orbRes = await serve.api.get(`/api/projects/${slug}/orb`);
    if (!orbRes.ok()) throw new Error(`orb detail: ${orbRes.status()} ${await orbRes.text()}`);
    const baseline = ((await orbRes.json()) as { orb: { image: string } }).orb.image;
    await page.goto(`${serve.url}/#/projects`);
    const probe = await page.context().newPage();
    await probe.goto(`${serve.url}/#/projects`);
    const c: Ctx = { page, probe, serve, slug, alphaName: 'Alpha', betaName: 'Beta', building: false, running: false, baseline };
    await expect(section(page, slug).locator('h2')).toHaveText('Alpha');
    await expect(section(probe, slug).locator('h2')).toHaveText('Alpha');
    return c;
  },

  actions: {
    async Rename(c) {
      const sec = section(c.probe, c.slug);
      await sec.getByRole('button', { name: /^More actions for / }).click();
      await c.probe.getByRole('menuitem', { name: 'Rename…', exact: true }).click();
      const d = c.probe.getByRole('dialog');
      await expect(d).toBeVisible();
      await d.getByRole('textbox').fill(c.betaName);
      await d.getByRole('button', { name: 'Rename', exact: true }).click();
      await expect(d).toBeHidden();
      await expect(section(c.probe, c.slug).locator('h2')).toHaveText('Beta');
    },
    // page's own list poll (app.tsx, projects re-read every 30 s): pollStepMs
    // is 0, so only this action ever moves page's cached render forward.
    UiPoll: (c) => c.page.clock.fastForward(31_000),
    async StartOrbBuild(c) { c.building = true; },
    async FinishOrbBuild(c) { c.building = false; c.running = true; },
    async StopOrb(c) { c.running = false; },
  },

  read: readUiState,
  status: (c) => section(c.page, c.slug).locator('h2'),
  sessions: () => [],
  // The list poll is a scripted action of this spec: a read that moved the
  // clock on its own would run it between steps.
  pollStepMs: 0,
  async cleanup(c) {
    await c.probe.close();
  },
});
