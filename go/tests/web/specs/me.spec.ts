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
    items: [
      { kind: 'needs-you', source: 'gh', title: 'Review comment on the demand fix', note: 'Priya', url: 'https://github.com/asi/uni-nes/pull/7801', at: new Date().toISOString(), cite: 'gh:asi/uni-nes#7801', repo: 'asi/uni-nes', author: 'priya' },
      { kind: 'moving', source: 'gh', title: 'Team review request in uni-tmi', url: 'https://github.com/asi/uni-tmi/pull/1575', at: new Date().toISOString(), cite: 'gh:asi/uni-tmi#1575', repo: 'asi/uni-tmi', author: 'someone' },
    ],
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

// Triage and steering are files: a dismissal lands in triage.json and,
// with a rule, in the profile's Not mine; a sentence lands under Watch
// or Not mine. The page hides and reorders at once, before any brief.
test('dismiss, pin and steer write the triage file and the profile, and the page shows it at once', async ({ serve, page }) => {
  await page.goto(serve.url + '/#/me');
  const tmi = page.locator('.me-sig-wrap', { hasText: 'Team review request in uni-tmi' });
  await expect(tmi).toBeVisible();
  // Dismiss with a rule about the repo.
  await tmi.hover();
  await tmi.getByRole('button', { name: 'Dismiss Team review request in uni-tmi' }).click();
  await page.getByRole('menuitem', { name: 'Nothing from asi/uni-tmi' }).click();
  await expect(page.getByText('Team review request in uni-tmi')).toHaveCount(0);
  await expect(page.getByText('1 dismissed row hidden')).toBeVisible();
  const me = path.join(serve.home, '.bough', 'wiki', 'topics', 'me');
  await expect.poll(() => JSON.parse(fs.readFileSync(path.join(me, 'triage.json'), 'utf8')).dismissed['gh:asi/uni-tmi#1575']).toBeTruthy();
  await expect.poll(() => fs.readFileSync(path.join(me, 'profile.md'), 'utf8')).toContain('nothing from asi/uni-tmi');

  // Pin the other: it leads in a Pinned group.
  const demand = page.locator('.me-sig-wrap', { hasText: 'Review comment on the demand fix' });
  await demand.hover();
  await demand.getByRole('button', { name: 'Pin Review comment on the demand fix' }).click();
  await expect(page.locator('.me-group[data-kind="pinned"]')).toContainText('Review comment on the demand fix');
  await expect.poll(() => JSON.parse(fs.readFileSync(path.join(me, 'triage.json'), 'utf8')).pinned).toContain('gh:asi/uni-nes#7801');

  // Steer: an exclusion files under Not mine, and the page says so.
  const box = page.getByRole('textbox', { name: 'Tell the brief what to watch or ignore' });
  await box.fill('ignore anything in uni-route-availability');
  await expect(page.getByRole('button', { name: 'Add to Not mine' })).toBeVisible();
  await page.getByRole('button', { name: 'Add to Not mine' }).click();
  await expect(page.getByText('Filed under Not mine. The next brief reads it.')).toBeVisible();
  await expect.poll(() => fs.readFileSync(path.join(me, 'profile.md'), 'utf8')).toContain('ignore anything in uni-route-availability');
});
