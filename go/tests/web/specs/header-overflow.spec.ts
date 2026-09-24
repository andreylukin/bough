// The session header at laptop widths: the Work button stays on screen
// with every chip present, low-priority chips fold into "…", and the Work
// popover stays inside the viewport. The session is mocked so the chips
// are exact; everything else is a real `bough serve` on an isolated HOME.
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort } from '../helpers/bough';

const test = base.extend<{ serve: string }>({
  serve: async ({ request }, use) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-head-'));
    const addr = `127.0.0.1:${await freePort()}`;
    const child = spawn(boughBin, ['serve', '--run', addr], {
      cwd: home, env: { ...process.env, HOME: home }, stdio: 'ignore',
    });
    try {
      await expect.poll(async () => {
        try {
          const token = fs.readFileSync(path.join(home, '.bough', 'serve.token'), 'utf8').trim();
          return (await request.get(`http://${addr}/api/health`, { headers: { Authorization: `Bearer ${token}` } })).status();
        } catch { return 0; }
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
const lead = {
  ...base_, id: 'p-lead', title: 'Split the serve API into handlers per resource', status: 'idle', live: true, entries: 5, mode: 'local',
  model: 'anthropic/claude-opus-5', agents: { running: 0, queued: 0, total: 1 },
  cache: { at: now, ttl: 3600, read: 90000, write: 2000, in: 100000 },
};
const kid = { ...base_, id: 'k-routes01', title: 'Map every route', status: 'done', live: false, entries: 6, mode: 'local', spawnedBy: 'p-lead' };
const entries = [
  { seq: 1, at: now, kind: 'prompt', text: 'split the API' },
  { seq: 2, at: now, kind: 'code', text: 'tools.bash("go test ./...")' },
  { seq: 3, at: now, kind: 'result', text: 'ok', data: { exit: 0 } },
  { seq: 4, at: now, kind: 'text', text: 'Done.' },
  { seq: 5, at: now, kind: 'done', text: '', data: { usage: { in: 123456, out: 4567, cost: 1.23, last_in: 98765 } } },
];

async function mock(page: Page) {
  await page.route(/\/api\/sessions(\?.*)?$/, (route) =>
    route.request().method() === 'GET' ? route.fulfill({ json: { sessions: [lead, kid] } }) : route.continue());
  await page.route(/\/api\/sessions\/[^/?]+(\?.*)?$/, (route) => {
    const id = new URL(route.request().url()).pathname.split('/').pop();
    return id === lead.id ? route.fulfill({ json: { session: lead, entries } }) : route.continue();
  });
  await page.route(/\/api\/sessions\/[^/]+\/children$/, (route) => route.fulfill({ json: { children: [kid] } }));
  await page.route(/\/api\/sessions\/[^/]+\/edits$/, (route) =>
    route.fulfill({ json: { repo: true, files: [{ path: 'go/internal/serve/api.go', add: 120, del: 40 }] } }));
  await page.route(/\/api\/models$/, (route) =>
    route.fulfill({ json: { providers: [{ plugin: 'llm-anthropic', models: [{ id: 'anthropic/claude-opus-5', context: 1000000 }] }], efforts: [] } }));
  await page.route(/\/api\/sessions\/[^/]+\/events$/, (route) => route.abort());
}

for (const [width, height] of [[1382, 900], [1024, 800]] as const) {
  test(`Work button and popover stay on screen at ${width}x${height}`, async ({ serve, page }, info) => {
    await mock(page);
    await page.setViewportSize({ width, height });
    await page.goto(serve + '/#/s/p-lead');
    const btn = page.locator('button.work-summary');
    await expect(btn).toBeVisible();
    await expect(page.locator('.thread-head .runtime-strip')).toContainText('Context');
    await page.screenshot({ path: info.outputPath(`header-${width}.png`) });
    const b = (await btn.boundingBox())!;
    expect(b.x).toBeGreaterThanOrEqual(0);
    expect(b.y).toBeGreaterThanOrEqual(0);
    expect(b.x + b.width).toBeLessThanOrEqual(width);
    expect(b.y + b.height).toBeLessThanOrEqual(height);
    // The chips never run over the title.
    await expect.poll(() => page.evaluate(() => {
      const main = document.querySelector('.thread-head .head-main')!.getBoundingClientRect();
      const strip = document.querySelector('.thread-head .runtime-strip')!.getBoundingClientRect();
      return strip.top >= main.bottom - 1 || strip.left >= main.right - 1;
    })).toBe(true);
    if (width === 1382) {
      // Folded chips are one click away.
      await page.locator('.rt-more>summary').click();
      await expect(page.locator('.rt-more .rt-pop .rt-cache-hot .rt-label')).toHaveText('Cache');
      await page.keyboard.press('Escape');
    }

    await btn.click();
    const pop = page.locator('.work-popover');
    await expect(pop).toContainText('Map every route');
    await page.screenshot({ path: info.outputPath(`work-${width}.png`) });
    const p = (await pop.boundingBox())!;
    expect(p.x).toBeGreaterThanOrEqual(0);
    expect(p.x + p.width).toBeLessThanOrEqual(width);
  });
}
