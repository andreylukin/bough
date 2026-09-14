// Background agents in the control room: children nest under their
// parent, the parent's head counts running agents, the child links back,
// and archiving a parent with running agents asks about stopping them.
// The session list is mocked so the tree is exact; everything else is a
// real `bough serve` on an isolated HOME.
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort } from '../helpers/bough';

const test = base.extend<{ serve: string }>({
  serve: async ({ request }, use) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-bg-'));
    const addr = `127.0.0.1:${await freePort()}`;
    const child = spawn(boughBin, ['serve', '--run', addr], {
      cwd: home, env: { ...process.env, HOME: home }, stdio: 'ignore',
    });
    try {
      await expect.poll(async () => {
        try { return (await request.get(`http://${addr}/api/health`)).status(); } catch { return 0; }
      }).toBe(200);
      await use(`http://${addr}`);
    } finally {
      if (child.exitCode === null && child.signalCode === null) {
        await new Promise<void>((resolve) => {
          const timer = setTimeout(() => child.kill('SIGKILL'), 3000);
          child.once('exit', () => { clearTimeout(timer); resolve(); });
          child.kill('SIGTERM');
        });
      }
      fs.rmSync(home, { recursive: true, force: true });
    }
  },
});

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

test('children nest under the parent with a queued mark; head count lists them; child links back', async ({ serve, page }) => {
  await mock(page, []);
  await page.goto(serve + '/#/s/p-lead');
  const kids = page.getByRole('group', { name: 'Agents started by Split the serve API' });
  await expect(kids.locator('.row-title')).toHaveText(['Map every route', 'Write the docs']);
  await expect(kids.locator('[data-id="k-docs0003"]')).toContainText(/queued/i);

  const chip = page.locator('button.head-agents');
  await expect(chip).toHaveText('2 agents');
  await chip.click();
  const pop = page.getByRole('dialog', { name: 'Background agents' });
  await expect(pop.locator('.row-title')).toHaveText(['Map every route', 'Write the docs']);
  await pop.getByRole('button', { name: /Map every route/ }).click();

  const back = page.locator('.head-parent');
  await expect(back).toContainText('spawned by Split the serve API');
  await back.getByRole('button').click();
  await expect(page.locator('h1')).toHaveText('Split the serve API');
});

for (const [pick, want] of [['Stop and archive', { stopChildren: true }], ['Archive only', {}]] as const) {
  test(`archiving a parent with running agents: ${pick}`, async ({ serve, page }) => {
    const archived: unknown[] = [];
    await mock(page, archived);
    await page.goto(serve + '/#/s/p-lead');
    await page.getByRole('button', { name: 'Session settings' }).click();
    await page.locator('.head-pop-item', { hasText: /^Archive$/ }).click();
    await expect(page.getByText('Stop its 1 running and 1 queued agents too?')).toBeVisible();
    await page.getByRole('button', { name: pick, exact: true }).click();
    await expect.poll(() => archived).toEqual([want]);
  });
}

test('phone width shows the parent count and child backlink', async ({ serve, page }, info) => {
  await mock(page, []);
  await page.setViewportSize({ width: 400, height: 800 });
  await page.goto(serve + '/#/s/p-lead');
  await expect(page.locator('button.head-agents')).toHaveText('2 agents');
  await page.screenshot({ path: info.outputPath('phone-parent.png') });
  await page.goto(serve + '/#/s/k-routes01');
  await expect(page.locator('.head-parent')).toContainText('spawned by');
  await page.screenshot({ path: info.outputPath('phone-child.png') });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
});
