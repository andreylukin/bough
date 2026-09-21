// The Me page against a real `bough serve` on an isolated HOME: the brief
// under topics/me is rendered as prose with its citations linked, the
// signals under it, and Refresh asks serve to write the brief now.
// Refresh is the one mocked call: writing a brief for real runs a model.
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort } from '../helpers/bough';

const today = new Date().toISOString().slice(0, 10);
const seed: Record<string, string> = {
  '.bough/wiki/topics/me/profile.md': '# Me\n\nSRE.\n',
  [`.bough/wiki/topics/me/briefs/${today}.md`]:
    '# Brief\n\nToday is about the demand fix. `gh:asi/uni-nes#7801`\n\n## Since yesterday\n\n- Put up the settlement fix. `gh:asi/uni-nes#7801`\n\n## Today\n\n- Chasing the review. `linear:NME-1462`\n',
  '.bough/wiki/topics/me/signals.json': JSON.stringify({
    asOf: new Date().toISOString(),
    items: [{ kind: 'needs-you', source: 'gh', title: 'Review comment on the demand fix', note: 'Priya', url: 'https://github.com/asi/uni-nes/pull/7801', at: new Date().toISOString() }],
    sources: [{ name: 'gh', ok: true }, { name: 'slack', ok: false, error: 'not connected' }],
  }),
  '.bough/wiki/index.md': '# Wiki index\n',
};

interface Serve { url: string; home: string; token: string }

const test = base.extend<{ serve: Serve }>({
  serve: async ({ request }, use) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-me-'));
    for (const [rel, text] of Object.entries(seed)) {
      const p = path.join(home, rel);
      fs.mkdirSync(path.dirname(p), { recursive: true });
      fs.writeFileSync(p, text);
    }
    const addr = `127.0.0.1:${await freePort()}`;
    const child = spawn(boughBin, ['serve', '--run', addr], { cwd: home, env: { ...process.env, HOME: home }, stdio: 'ignore' });
    let token = '';
    try {
      await expect.poll(async () => {
        try {
          token = fs.readFileSync(path.join(home, '.bough', 'serve.token'), 'utf8').trim();
          return (await request.get(`http://${addr}/api/health`, { headers: { Authorization: `Bearer ${token}` } })).status();
        } catch { return 0; }
      }).toBe(200);
      await use({ url: `http://${addr}`, home, token });
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

test('the brief renders with linked citations, the signals under it, and Refresh asks for a new one', async ({ serve, page }) => {
  let refreshed = 0;
  await page.route(/\/api\/me\/refresh$/, (route) => { refreshed++; return route.fulfill({ json: { ok: true } }); });
  await page.goto(serve.url + '/#/me');
  await expect(page.locator('.me-brief')).toContainText('Today is about the demand fix.');
  await expect(page.locator('.me-brief a.wk-cite-ext').first()).toHaveAttribute('href', 'https://github.com/asi/uni-nes/issues/7801');
  await expect(page.locator('.me-brief').getByText('NME-1462')).toBeVisible();
  await expect(page.locator('.me-group[data-kind="needs-you"]')).toContainText('Review comment on the demand fix');
  await expect(page.locator('.me-rail')).toContainText('not connected');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect.poll(() => refreshed).toBe(1);
  await expect(page.getByRole('button', { name: 'Writing…' })).toBeDisabled();
  // The nav has it, and the name leads there.
  await page.goto(serve.url + '/#/');
  await page.locator('.side-nav').getByRole('link', { name: 'Me' }).click();
  await expect.poll(() => page.evaluate(() => window.location.hash)).toBe('#/me');
});

test('no profile: the page says what to write and never offers Refresh', async ({ serve, page }) => {
  fs.rmSync(path.join(serve.home, '.bough', 'wiki', 'topics', 'me', 'profile.md'));
  await page.goto(serve.url + '/#/me');
  await expect(page.getByText('Tell the brief whose work this is')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Refresh' })).toHaveCount(0);
});
