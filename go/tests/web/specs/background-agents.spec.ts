// Background agents in the control room: children nest under their
// parent, the parent's head counts running agents, the child links back,
// and archiving a parent with running agents asks about stopping them.
// The session list is mocked so the tree is exact; everything else is
// the worker's shared `bough serve` (nothing here writes to it).
import type { Page } from '@playwright/test';
import { test, expect } from '../helpers/serve';

const now = new Date().toISOString();
const base_ = { cwd: '/w/bough', repo: 'andreylukin/bough', branch: 'main', archived: false, modified: now, lastAt: now };
const rows = [
  { ...base_, id: 'p-lead', title: 'Split the serve API', status: 'running', live: true, entries: 20, mode: 'local',
    agents: { running: 1, queued: 1, total: 2 } },
  { ...base_, id: 'k-routes01', title: 'Map every route', status: 'running', live: true, entries: 6, mode: 'local', spawnedBy: 'p-lead' },
  { ...base_, id: 'k-docs0003', title: 'Write the docs', status: 'queued', queued: true, live: false, entries: 0, mode: 'project', spawnedBy: 'p-lead' },
];

async function mock(page: Page, archived: unknown[]) {
  await page.route(/\/api\/sessions(\?.*)?$/, (route) =>
    route.request().method() === 'GET' ? route.fulfill({ json: { sessions: rows } }) : route.continue());
  await page.route(/\/api\/sessions\/[^/?]+(\?.*)?$/, (route) => {
    const id = new URL(route.request().url()).pathname.split('/').pop();
    const row = rows.find((r) => r.id === id);
    return row ? route.fulfill({ json: { session: row, entries: [] } }) : route.continue();
  });
  await page.route(/\/api\/sessions\/[^/]+\/children$/, (route) =>
    route.fulfill({ json: { children: rows.filter((r) => r.spawnedBy === 'p-lead') } }));
  await page.route(/\/api\/sessions\/[^/]+\/events$/, (route) => route.abort());
  await page.route(/\/api\/sessions\/[^/]+\/archive$/, (route) => {
    archived.push(route.request().postData() ? JSON.parse(route.request().postData()!) : {});
    return route.fulfill({ json: { ok: true } });
  });
}

test('children fold into the parent row and its Work dialog; child links back', async ({ sharedServe, page }) => {
  await mock(page, []);
  await page.goto(sharedServe.url + '/#/s/p-lead');
  // Agents are not sidebar rows: the parent's row counts them, Work lists them.
  const tree = page.getByRole('tree', { name: 'Sessions' });
  await expect(tree.getByRole('treeitem', { name: /^Split the serve API,.*background: 1 running/ })).toBeVisible();
  await expect(tree.getByRole('treeitem', { name: /^Map every route/ })).toHaveCount(0);

  await page.locator('button.work-summary').click();
  const work = page.getByRole('dialog').filter({ hasText: 'Map every route' });
  await expect(work).toContainText('Write the docs');

  await page.goto(sharedServe.url + '/#/s/k-routes01');
  const back = page.locator('.child-parent-link');
  await expect(back).toContainText('Parent: Split the serve API');
  await back.click();
  await expect(page.locator('h1')).toHaveText('Split the serve API');
});

for (const [pick, want] of [['Stop and archive', { stopChildren: true }], ['Archive only', {}]] as const) {
  test(`archiving a parent with running agents: ${pick}`, async ({ sharedServe, page }) => {
    const archived: unknown[] = [];
    await mock(page, archived);
    await page.goto(sharedServe.url + '/#/s/p-lead');
    await page.getByRole('button', { name: 'Session settings' }).click();
    await page.locator('.head-pop-item', { hasText: /^Archive…$/ }).click();
    await expect(page.getByText(/^Stop its .+ too\?$/)).toBeVisible();
    await page.getByRole('button', { name: pick, exact: true }).click();
    await expect.poll(() => archived).toEqual([want]);
  });
}

test('phone width shows the parent count and child backlink', async ({ sharedServe, page }, info) => {
  await mock(page, []);
  await page.setViewportSize({ width: 400, height: 800 });
  await page.goto(sharedServe.url + '/#/s/p-lead');
  await expect(page.locator('button.work-summary')).toBeVisible();
  await page.screenshot({ path: info.outputPath('phone-parent.png') });
  await page.goto(sharedServe.url + '/#/s/k-routes01');
  await expect(page.locator('.child-parent-link')).toContainText('Parent:');
  await page.screenshot({ path: info.outputPath('phone-child.png') });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
});
