// go/tests/model/specs/mode_picker_effort_model_race.fizz in the
// browser: the model/effort picker (mode.tsx) racing a provider change
// underneath it — a config hot reload that remounts the session's own
// llm row (see mbt/mode_picker_effort_model_race_test.go for the same
// race played over raw HTTP). Every generated path is walked through
// the page against a real serve.
//
// As that Go adapter's own comment explains: GET /api/models never
// depends on the session's own provider, and the shipped front end
// fetches it exactly once per page load (app.tsx's useCatalogue keys
// only on its own retry nonce) — there is no code path that refetches
// it when a session's config changes underneath it. So, precisely as
// the Go adapter keeps catalogue/switching/epoch/pick_epoch/stale as
// its own bookkeeping rather than reading them off the server (the
// README's "tracked by the adapter" allowance, the same one
// example.spec.ts uses for `viewing`), this walk keeps the same fields
// as its own bookkeeping and derives shown/group/offers_default/efforts
// from them with the identical `view()` the spec and the Go adapter
// both use. What is real and driven through the page or a real
// second-caller request on every step: the one genuine GET /api/models
// round trip, the session's own bough.yml actually being rewritten and
// its `configured` row actually changing underneath it, the POST
// /model that a pick really sends (held on its way out and released
// like model-effort-controls.spec.ts's Land, so a provider change in
// between never stops it from landing), and the effort actually
// selected, read back off the Select's own accessible value.
import * as fs from 'fs';
import * as path from 'path';
import type { Locator, Page, Request, Route } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const SERVE_CONFIG = '- id: llm\n  plugin: llm-echo\n';
// The session's own bough.yml: same plugin (never needs a key), a
// different `model:` per provider, so ProviderChanges is a real hot
// reload and row.configured really flips.
// configuredIn (cmd/bough/serve.go) memoizes by the file's mtime and
// size, and two rewrites close together can tie on a coarse filesystem
// clock (AGENTS.md's mtime trap) — so the two variants differ in size
// too, never only in mtime, to force a real cache miss.
function sessionConfig(providerB: boolean): string {
  return `- id: llm\n  plugin: llm-echo\n  config:\n    model: "${providerB ? 'provider-b-changed' : 'provider-a'}"\n`;
}

// The spec's x: an id no provider lists.
const UNLISTED = 'unlisted-model-x';

interface Catalogue { providers: { plugin: string; models?: { id: string; efforts?: string[] }[] }[]; efforts: string[] }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  providerB: boolean;
  // The page's one real GET /api/models, held until CatalogueLoads
  // answers it; empty forever after (the product never asks again).
  catalogues: Route[];
  firstLoadDone: boolean;
  // The spec's m: a catalogue model with its own levels, including high.
  m?: { id: string; plugin: string; efforts: string[] };
  // The picker's POST /model, held until Land.
  landing?: Route;
  // Bookkeeping mirroring specs/mode_picker_effort_model_race.fizz and
  // mbt/mode_picker_effort_model_race_test.go's adapter exactly.
  catalogue: 'loading' | 'loaded' | 'stale';
  runs: 'cfg' | 'm' | 'x';
  switching: boolean;
  epoch: boolean;
  pickEpoch: boolean;
  stale: boolean;
  effort: string;
}

const tools = (c: Ctx) => c.page.locator('.composer-tools');
const modelSel = (c: Ctx) => tools(c).locator('.sel').filter({ has: c.page.locator('button[aria-label^="Next turn model"]') });
const effortSel = (c: Ctx) => tools(c).locator('.sel').filter({ has: c.page.locator('button[aria-label^="Next turn effort"]') });
const modelBtn = (c: Ctx) => modelSel(c).locator('button.sel-btn');
const effortBtn = (c: Ctx) => effortSel(c).locator('button.sel-btn');

// The text after "<label>: " in a Select button's accessible name.
async function buttonValue(b: Locator): Promise<string> {
  const name = (await b.getAttribute('aria-label')) ?? '';
  return name.slice(name.indexOf(': ') + 2);
}

async function waitFor(ok: () => Promise<boolean> | boolean, what: string, ms = 10_000): Promise<void> {
  const deadline = Date.now() + ms;
  for (;;) {
    if (await ok()) return;
    if (Date.now() > deadline) throw new Error(`mode_picker_effort_model_race: waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

async function waitConfigured(c: Ctx, model: string): Promise<void> {
  await waitFor(async () => {
    const res = await c.serve.api.get(`/api/sessions/${c.id}`);
    // GET /api/sessions/<id> answers {entries, session: {...}}: the row
    // (and so `configured`) is nested under `session`, unlike the list
    // endpoint's flat rows.
    const body = (await res.json()) as { session?: { configured?: { model?: string } } };
    return body.session?.configured?.model === model;
  }, `the config reload to land on ${model}`, 15_000);
}

// specs/mode_picker_effort_model_race.fizz's own view(): what the picker
// shows, given whether the catalogue is loaded, what the next turn runs
// and whether that pick landed under a provider epoch since moved on.
function view(catalogue: string, runs: string, stale: boolean): { shown: string; group: string; offersDefault: boolean; efforts: string } {
  if (catalogue !== 'loaded') return { shown: '', group: 'none', offersDefault: false, efforts: 'none' };
  const offersDefault = runs === 'cfg';
  let group: string;
  if (runs === 'cfg') group = 'configured';
  else if (runs === 'm' && !stale) group = 'catalogue';
  else group = 'inuse';
  const efforts = runs === 'm' && !stale ? 'own' : 'all';
  return { shown: runs, group, offersDefault, efforts };
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const v = view(c.catalogue, c.runs, c.stale);
  return {
    catalogue: c.catalogue,
    runs: c.runs,
    shown: v.shown,
    group: v.group,
    offers_default: v.offersDefault,
    efforts: v.efforts,
    effort: c.effort,
    switching: c.switching,
    stale: c.stale,
    epoch: c.epoch,
    pick_epoch: c.pickEpoch,
  };
}

modelTests<Ctx>({
  spec: 'mode_picker_effort_model_race',
  role: 'Session#0',
  config: SERVE_CONFIG,
  shared: true,
  reset: true,

  async init(page, serve) {
    const c: Ctx = {
      page, serve, id: '', providerB: false, catalogues: [], firstLoadDone: false,
      catalogue: 'loading', runs: 'cfg', switching: false, epoch: false, pickEpoch: false, stale: false, effort: '',
    };
    fs.writeFileSync(path.join(serve.work, 'bough.yml'), sessionConfig(false));
    c.id = await serve.newSession();
    await page.route('**/api/models', (route) => {
      if (route.request().method() !== 'GET') return route.continue();
      c.catalogues.push(route);
    });
    await page.route(`**/api/sessions/${c.id}/model`, (route, req: Request) => {
      if (req.method() !== 'POST') return route.continue();
      c.landing = route;
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await modelBtn(c).waitFor();
    await waitFor(() => c.catalogues.length > 0, 'the page\'s GET /api/models');
    return c;
  },

  actions: {
    async CatalogueLoads(c) {
      if (!c.firstLoadDone) {
        const route = c.catalogues.shift();
        if (!route) throw new Error('no GET /api/models in flight');
        const res = await route.fetch();
        const cat = (await res.json()) as Catalogue;
        const all = cat.providers.flatMap((p) => p.models?.map((m) => ({ ...m, plugin: p.plugin })) ?? []);
        const m = all.find((x) => x.efforts?.includes('high') && JSON.stringify(x.efforts) !== JSON.stringify(cat.efforts)
          && all.filter((y) => y.id === x.id).length === 1);
        if (!m) throw new Error('the catalogue lists no model with its own levels including high');
        c.m = { id: m.id, plugin: m.plugin, efforts: m.efforts ?? [] };
        c.firstLoadDone = true;
        await route.fulfill({ response: res, json: cat });
      } else {
        // The product never re-fetches on its own; a real health check
        // still stands in for "the catalogue answers again".
        const res = await c.serve.api.get('/api/models');
        if (!res.ok()) throw new Error(`GET /api/models: ${res.status()}`);
      }
      c.catalogue = 'loaded';
    },

    // A real hot reload of the session's own bough.yml. Before any pick,
    // that is row.configured really flipping underneath whatever is in
    // flight; once a model was ever set (runs != "cfg"), row.configured
    // is nil for good ("nothing switches back to it", internal/serve/
    // api.go), so there is nothing left to observe the reload by, and
    // this is the walk's own bookkeeping like the Go adapter's epoch.
    async ProviderChanges(c) {
      c.providerB = !c.providerB;
      fs.writeFileSync(path.join(c.serve.work, 'bough.yml'), sessionConfig(c.providerB));
      if (c.runs === 'cfg') await waitConfigured(c, c.providerB ? 'provider-b-changed' : 'provider-a');
      c.epoch = !c.epoch;
      c.catalogue = 'stale';
    },

    // The page noticing its catalogue no longer describes the current
    // provider and asking for it again: nothing on screen names this
    // (see the file header), so it is the walk's own bookkeeping, same
    // as the Go adapter's CatalogueReload.
    async CatalogueReload(c) {
      c.catalogue = 'loading';
    },

    async PickModel(c) {
      c.pickEpoch = c.epoch;
      await modelBtn(c).click();
      await modelSel(c).locator('input.sel-search').fill(c.m!.id);
      await modelSel(c).getByRole('option').filter({ has: c.page.locator('.sel-label', { hasText: new RegExp(`^${c.m!.id}$`) }) }).click();
      await waitFor(() => !!c.landing, 'the picker POST /model');
      c.switching = true;
    },

    async Land(c) {
      const route = c.landing!;
      c.landing = undefined;
      c.switching = false;
      c.stale = c.pickEpoch !== c.epoch;
      c.runs = 'm';
      await route.continue();
      await waitFor(async () => (await buttonValue(modelBtn(c))) === c.m!.id, 'the picked model to show');
    },

    // No UI names a model the catalogue does not list: another client.
    async SetUnlistedModel(c) {
      const res = await c.serve.api.post(`/api/sessions/${c.id}/model`, { data: { model: UNLISTED } });
      if (!res.ok()) throw new Error(`model: ${res.status()} ${await res.text()}`);
      c.runs = 'x';
      await waitFor(async () => (await buttonValue(modelBtn(c))) === UNLISTED, 'the unlisted model to show');
    },

    async PickEffort(c) {
      await effortBtn(c).click();
      await effortSel(c).getByRole('option', { name: 'High', exact: true }).click();
      await waitFor(async () => (await buttonValue(effortBtn(c))) === 'High', 'the effort pick to show');
      c.effort = 'high';
    },
  },

  read: readUiState,
  status: modelBtn,
  sessions: (c) => [c.id],
  async cleanup(c) {
    // A held POST would keep the page's request open past the test.
    await c.landing?.continue().catch(() => {});
  },
});
