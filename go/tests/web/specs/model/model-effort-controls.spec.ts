// go/tests/model/specs/model_effort_controls.fizz in the browser: the
// "Next turn" model and effort pickers in one session's composer. Every
// generated path is walked through the page against a real serve, and at
// each node readUiState must equal the spec's state (recipe:
// go/tests/model/README.md).
//
// As in the Go adapter (mbt/model_effort_controls_test.go), serve and the
// session run different llm rows, the way a serve started from ~ runs
// sessions in a repo with its own bough.yml: serve's config names llm-echo
// with a model, the session's cwd names llm-control with none. A picker
// that names serve's model is the bug this flow was written for.
import * as fs from 'fs';
import * as path from 'path';
import type { ConsoleMessage, Locator, Page, Request, Route } from '@playwright/test';
import { controlDir, queue } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const SERVE_CONFIG = '- id: llm\n  plugin: llm-echo\n  config:\n    model: serve-default\n';
const SESSION_CONFIG = '- id: llm\n  plugin: llm-control\n';
// The spec's x: an id no provider lists.
const UNLISTED = 'unlisted-model-x';

interface Catalogue { providers: { plugin: string; models?: { id: string; efforts?: string[] }[] }[]; efforts: string[] }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  // The page's GET /api/models calls, held until the spec says how they
  // answer (CatalogueLoads | CatalogueFails); `cat` once one loaded.
  catalogues: Route[];
  cat?: Catalogue;
  // The spec's m: a catalogue model with its own levels, including high.
  m?: { id: string; efforts: string[] };
  // The picker's POST /model, held until Land.
  landing?: Route;
  // Land on an archived session: the page's POST is refused (409) as the
  // spec says, and Chrome logs every 4xx response as a console error.
  refused: boolean;
  // What the walk asked the next turn to run (cfg | m | x). The page
  // shows it only once the catalogue loads, so like the Go adapter the
  // walk keeps it; `shown` is the page's claim, and the spec's
  // PickerNamesWhatRuns makes the two agree wherever the page names one.
  runs: string;
  turn: number;
}

const tools = (c: Ctx) => c.page.locator('.composer-tools');
const modelSel = (c: Ctx) => tools(c).locator('.sel').filter({ has: c.page.locator('button[aria-label^="Next turn model"]') });
const effortSel = (c: Ctx) => tools(c).locator('.sel').filter({ has: c.page.locator('button[aria-label^="Next turn effort"]') });
const modelBtn = (c: Ctx) => modelSel(c).locator('button.sel-btn');
const effortBtn = (c: Ctx) => effortSel(c).locator('button.sel-btn');

// The effort menu's words for the API's levels (app.tsx effortLabel).
function effortLabel(e: string): string {
  const words: Record<string, string> = { xhigh: 'Extra high', minimal: 'Minimal', max: 'Max' };
  return words[e] ?? e.charAt(0).toUpperCase() + e.slice(1);
}

// The spec's name for a model the picker names.
function modelName(c: Ctx, label: string): string {
  if (label === 'llm-control' || label === 'control') return 'cfg';
  if (c.m && label === c.m.id) return 'm';
  if (label === UNLISTED) return 'x';
  return `other: ${label}`;
}

// The text after "<label>: " in a Select button's accessible name.
async function buttonValue(b: Locator): Promise<string> {
  const name = (await b.getAttribute('aria-label')) ?? '';
  return name.slice(name.indexOf(': ') + 2);
}

// Opens sel's list (unless a pick already holds it open), runs read on
// it, and leaves it as it was.
async function withList<T>(sel: Locator, read: (list: Locator, search: Locator) => Promise<T>): Promise<T> {
  const btn = sel.locator('button.sel-btn');
  const wasOpen = (await btn.getAttribute('aria-expanded')) === 'true';
  if (!wasOpen) await btn.click();
  try {
    return await read(sel.locator('[role="listbox"]'), sel.locator('input.sel-search'));
  } finally {
    const search = sel.locator('input.sel-search');
    if (await search.count()) await search.fill('');
    if (!wasOpen) await btn.press('Escape');
  }
}

// Each option's label with the group heading it sits under. Searching
// shows options under their own groups; with no query the current value
// leads under "Next turn" instead.
function grouped(list: Locator): Promise<{ label: string; group: string; selected: boolean }[]> {
  return list.evaluate((el) => {
    const out: { label: string; group: string; selected: boolean }[] = [];
    let group = '';
    for (const n of Array.from(el.children)) {
      if (n.classList.contains('sel-group')) group = n.textContent ?? '';
      else if (n.getAttribute('role') === 'option') {
        out.push({ label: n.querySelector('.sel-label')?.textContent ?? '', group, selected: n.getAttribute('aria-selected') === 'true' });
      }
    }
    return out;
  });
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const retry = tools(c).getByRole('button', { name: 'Models unavailable · Retry' });
  const effortText = (await effortSel(c).locator('.sel-value').textContent()) ?? '';
  const catalogue = (await retry.isVisible()) ? 'failed' : effortText === 'Loading' ? 'loading' : 'loaded';

  // The model picker: what the button names, the picked option's group,
  // and whether a Configured option is offered.
  const named = !(await modelSel(c).locator('.sel-value.sel-placeholder').count());
  const label = named ? await buttonValue(modelBtn(c)) : '';
  let group = 'none';
  let offersDefault = false;
  if (await modelBtn(c).isEnabled()) {
    ({ group, offersDefault } = await withList(modelSel(c), async (list, search) => {
      let g = 'none';
      if (label) {
        await search.fill(label);
        const heading = (await grouped(list)).find((o) => o.selected)?.group ?? '';
        g = heading === '' ? 'none' : heading === 'Configured' ? 'configured' : heading === 'In use' ? 'inuse' : 'catalogue';
      }
      await search.fill('configured');
      return { group: g, offersDefault: (await grouped(list)).some((o) => o.group === 'Configured') };
    }));
  }

  // The effort select: its levels, and its value.
  let efforts = 'none';
  if (await effortBtn(c).isEnabled()) {
    const levels = await withList(effortSel(c), async (list) =>
      (await grouped(list)).map((o) => o.label).filter((l) => !/default/i.test(l)));
    // As sets: the current level leads the list.
    const same = (want: string[] | undefined) => !!want && JSON.stringify([...levels].sort()) === JSON.stringify(want.map(effortLabel).sort());
    efforts = same(c.m?.efforts) ? 'own' : same(c.cat?.efforts) ? 'all' : `other: ${levels.join(',')}`;
  }
  const effortValue = await buttonValue(effortBtn(c));
  const effort = /default/i.test(effortValue) || effortValue === 'Loading' || effortValue === 'Unavailable'
    ? '' : (c.cat?.efforts.find((e) => effortLabel(e) === effortValue) ?? effortValue);

  return {
    catalogue,
    runs: c.runs,
    shown: label ? modelName(c, label) : '',
    group,
    offers_default: offersDefault,
    efforts,
    effort,
    archived: await c.page.locator('.archived-note').isVisible(),
    switching: await modelSel(c).getByRole('status').filter({ hasText: 'Saving' }).isVisible(),
  };
}

// The catalogue fetch the page is waiting on.
function heldCatalogue(c: Ctx): Route {
  const r = c.catalogues.shift();
  if (!r) throw new Error('no GET /api/models in flight');
  return r;
}

// The number of finished turns in the session's transcript.
async function dones(c: Ctx): Promise<number> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  const body = await res.json() as { entries?: { kind: string }[] };
  return (body.entries ?? []).filter((l) => l.kind === 'done').length;
}

// The session settings popover's item, e.g. "Archive…".
async function settings(c: Ctx, item: string): Promise<void> {
  await c.page.getByRole('button', { name: 'Session settings' }).click();
  await c.page.getByRole('dialog', { name: 'Session settings' }).getByRole('button', { name: item }).click();
}

// Another client's POST, made when the spec's action has no UI: the page
// learns of it from the list poll like any change made elsewhere.
async function post(c: Ctx, verb: string, body: unknown, archivedStatus = 409): Promise<void> {
  const res = await c.serve.api.post(`/api/sessions/${c.id}/${verb}`, { data: body });
  const archived = await c.page.locator('.archived-note').isVisible();
  const want = verb === 'effort' && (body as { effort: string }).effort === 'extreme' ? 500 : archived ? archivedStatus : 200;
  if (res.status() !== want) throw new Error(`POST ${verb} ${JSON.stringify(body)}: ${res.status()}, want ${want}: ${await res.text()}`);
}

modelTests<Ctx>({
  spec: 'model_effort_controls',
  role: 'Session#0',
  config: SERVE_CONFIG,
  shared: true,
  reset: true,

  async init(page, serve) {
    fs.writeFileSync(path.join(serve.work, 'bough.yml'), SESSION_CONFIG);
    const c: Ctx = { page, serve, id: await serve.newSession(), catalogues: [], runs: 'cfg', turn: 0, refused: false };
    // The harness's console listeners see everything but the one line the
    // spec's refused Land owes: Chrome's log of that 409 on this
    // session's /model. The page's own handling of it (the picker's
    // "Couldn't save") is still the page's, and any other error fails.
    for (const f of page.listeners('console') as ((m: ConsoleMessage) => void)[]) {
      page.off('console', f);
      page.on('console', (m) => {
        const refusal = c.refused && m.text().includes('status of 409') && m.location().url.endsWith(`/api/sessions/${c.id}/model`);
        if (!refusal) f(m);
      });
    }
    // Once one answered, later reads (another screen's) go through.
    await page.route('**/api/models', (route) => {
      if (c.cat) return route.fulfill({ json: c.cat });
      c.catalogues.push(route);
    });
    await page.route(`**/api/sessions/${c.id}/model`, (route, req: Request) => {
      if (req.method() !== 'POST') return route.continue();
      c.landing = route;
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await modelBtn(c).waitFor();
    await expectHeld(c);
    return c;
  },

  actions: {
    async CatalogueLoads(c) {
      const route = heldCatalogue(c);
      const res = await route.fetch();
      const cat = await res.json() as Catalogue;
      if (!c.m) {
        // Unique across providers, so the pick names one option.
        const all = cat.providers.flatMap((p) => p.models ?? []);
        const m = all.find((x) => x.efforts?.includes('high') && JSON.stringify(x.efforts) !== JSON.stringify(cat.efforts)
          && all.filter((y) => y.id === x.id).length === 1);
        if (!m) throw new Error('the catalogue lists no model with its own levels including high');
        c.m = { id: m.id, efforts: m.efforts ?? [] };
      }
      c.cat = cat;
      await route.fulfill({ response: res, json: cat });
    },
    // A proxy's error page where the JSON should be: the fetch itself
    // succeeds, so this is the page's own failure path, not the network's.
    async CatalogueFails(c) {
      await heldCatalogue(c).fulfill({ status: 200, contentType: 'text/html', body: '<html>Bad gateway</html>' });
    },
    async RetryCatalogue(c) {
      await tools(c).getByRole('button', { name: 'Models unavailable · Retry' }).click();
      await expectHeld(c);
    },
    async Turn(c) {
      const name = `me${String(++c.turn).padStart(4, '0')}`;
      const dir = controlDir(c.serve.home);
      // Queued whatever runs: on m (a provider with no key here) the turn
      // fails without taking it, and it is taken back below.
      queue(dir, name, { mode: 'ok', text: `answered ${name}` });
      const before = await dones(c);
      await c.page.locator('#composer').fill(`turn ${name}`);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      const deadline = Date.now() + 20_000;
      while (await dones(c) <= before) {
        if (Date.now() > deadline) throw new Error(`turn ${name} did not close`);
        await new Promise((r) => setTimeout(r, 100));
      }
      fs.rmSync(path.join(dir, name + '.json'), { force: true });
    },
    async PickModel(c) {
      await modelBtn(c).click();
      await modelSel(c).locator('input.sel-search').fill(c.m!.id);
      // The option's name carries its context size too.
      await modelSel(c).getByRole('option').filter({ has: c.page.locator('.sel-label', { hasText: new RegExp(`^${c.m!.id}$`) }) }).click();
      await waitFor(() => !!c.landing, 'the picker POST /model');
    },
    async Land(c) {
      const route = c.landing!;
      c.landing = undefined;
      if (await c.page.locator('.archived-note').isVisible()) c.refused = true;
      else c.runs = 'm';
      await route.continue();
    },
    // No UI names a model the catalogue does not list: another client.
    async SetUnlistedModel(c) {
      await post(c, 'model', { model: UNLISTED });
      if (!(await c.page.locator('.archived-note').isVisible())) c.runs = 'x';
    },
    async PickEffort(c) {
      await effortBtn(c).click();
      await effortSel(c).getByRole('option', { name: 'High', exact: true }).click();
    },
    // No UI offers a level the select does not list: another client.
    BadEffort: (c) => post(c, 'effort', { effort: 'extreme' }),
    async Archive(c) {
      await settings(c, 'Archive…');
      await c.page.getByRole('dialog').getByRole('button', { name: 'Archive', exact: true }).click();
    },
    Unarchive: (c) => settings(c, 'Unarchive'),
  },

  read: readUiState,
  status: modelBtn,
  sessions: (c) => [c.id],
  async cleanup(c) {
    // A held POST would keep the page's request open past the test.
    await c.landing?.continue().catch(() => {});
  },
});

async function expectHeld(c: Ctx): Promise<void> {
  await waitFor(() => c.catalogues.length > 0, 'the page\'s GET /api/models');
}

async function waitFor(ok: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + 10_000;
  while (!ok()) {
    if (Date.now() > deadline) throw new Error(`waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 20));
  }
}
