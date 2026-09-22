// The Rewrite page against a real `bough serve` on an isolated HOME: a
// pasted draft is split into sentences, each one is rewritten, kept or
// cut with the AI's note beside it, and the result is the person's own
// words in the draft's shape. The note is the one mocked call: a real
// note runs a model. A Notion link goes through /api/rewrite/fetch.
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort } from '../helpers/bough';

interface Serve { url: string; home: string }

const test = base.extend<{ serve: Serve }>({
  serve: async ({ request }, use) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-rw-'));
    const addr = `127.0.0.1:${await freePort()}`;
    const child = spawn(boughBin, ['serve', '--run', addr], { cwd: home, env: { ...process.env, HOME: home }, stdio: 'ignore' });
    try {
      await expect.poll(async () => {
        try {
          const token = fs.readFileSync(path.join(home, '.bough', 'serve.token'), 'utf8').trim();
          return (await request.get(`http://${addr}/api/health`, { headers: { Authorization: `Bearer ${token}` } })).status();
        } catch { return 0; }
      }).toBe(200);
      await use({ url: `http://${addr}`, home });
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

const draft = `# Problem

Our current retry mechanism leverages a fixed-interval strategy. This leads to a suboptimal experience for merchants. Specifically, transient errors are retried at the same cadence as hard declines.

- Furthermore, the absence of jitter creates thundering herds.
`;

test('a pasted draft is walked one sentence at a time and comes back in your words', async ({ serve, page }) => {
  const asked: string[] = [];
  await page.route(/\/api\/rewrite\/note$/, async (route) => {
    const body = route.request().postDataJSON() as { sentence: string; before: string; after: string };
    asked.push(body.sentence);
    await route.fulfill({ json: { claim: `claim of: ${body.sentence.slice(0, 12)}`, tells: body.sentence.includes('leverages') ? ['leverages'] : [], advice: 'Say it plainly.' } });
  });
  await page.goto(serve.url + '/#/rewrite');
  await page.getByRole('textbox', { name: 'Document or Notion link' }).fill(draft);
  await page.getByRole('button', { name: 'Start' }).click();

  // Sentence 1: the note arrives, the tell is marked, and the person writes.
  await expect(page.locator('.rw-sent')).toContainText('Our current retry mechanism leverages');
  await expect(page.locator('.rw-note')).toContainText('claim of: Our current');
  await expect(page.locator('mark.rw-tell')).toHaveText('leverages');
  await expect(page.locator('.rw-prog')).toContainText('0 / 4');
  // The next sentence's note is asked ahead of time.
  await expect.poll(() => asked.length).toBe(2);
  const box = page.getByRole('textbox', { name: 'Your version' });
  await box.fill('Retries run on a fixed clock.');
  await box.press('Meta+Enter');

  // Sentence 2: cut. Sentence 3: kept. The bullet: rewritten.
  await expect(page.locator('.rw-sent')).toContainText('suboptimal experience');
  await expect(page.locator('.rw-prog')).toContainText('1 / 4');
  await page.getByRole('button', { name: /^Cut/ }).click();
  await expect(page.locator('.rw-sent')).toContainText('Specifically');
  await page.getByRole('button', { name: /^Keep as is/ }).click();
  await expect(page.locator('.rw-sent')).toContainText('jitter');
  await box.fill('Without jitter every client retries at once.');
  await page.getByRole('button', { name: 'Next ⌘↩' }).click();

  // Done: the result keeps the heading and the bullet marker.
  await expect(page.getByText('All 4 sentences done')).toBeVisible();
  await expect(page.locator('.rw-result')).toHaveText(
    '# Problem\n\nRetries run on a fixed clock. Specifically, transient errors are retried at the same cadence as hard declines.\n\n- Without jitter every client retries at once.\n');
  await expect(page.locator('.rw-foot')).toContainText('2 rewritten · 1 kept · 1 cut');

  // The doc view lists every sentence with its fate, and a row reopens it.
  await page.getByRole('button', { name: 'Show doc' }).first().click();
  const rows = page.locator('.rw-doc-row');
  await expect(rows).toHaveCount(4);
  await expect(rows.nth(1).locator('.rw-doc-r')).toHaveText('cut');
  await expect(rows.nth(2).locator('.rw-doc-r')).toHaveText('kept as is');
  await rows.nth(0).click();
  await expect(box).toHaveValue('Retries run on a fixed clock.');

  // A reload lands on the same sentence with the same draft.
  await page.reload();
  await expect(page.locator('.rw-sent')).toContainText('Our current retry mechanism');
  await expect(box).toHaveValue('Retries run on a fixed clock.');

  // The nav has it, and the name leads there.
  await page.goto(serve.url + '/#/');
  await page.locator('.side-nav').getByRole('link', { name: 'Rewrite' }).click();
  await expect.poll(() => page.evaluate(() => window.location.hash)).toBe('#/rewrite');
});

test('a Notion link is read through the server, and a missing token says what to set', async ({ serve, page }) => {
  await page.goto(serve.url + '/#/rewrite');
  await page.evaluate(() => localStorage.clear());
  await page.reload();
  const box = page.getByRole('textbox', { name: 'Document or Notion link' });
  await box.fill('https://www.notion.so/acme/Retry-1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d');
  await expect(page.getByText('NOTION_TOKEN in ~/.bough/env')).toBeVisible();
  // No token on this HOME: the server's error is shown as is.
  await page.getByRole('button', { name: 'Read the page' }).click();
  await expect(page.getByRole('alert')).toContainText('NOTION_TOKEN is not set');
  // With the fetch answered, the page opens under the Notion title.
  await page.route(/\/api\/rewrite\/fetch$/, (route) => route.fulfill({ json: { title: 'Retry design', text: '## Why\n\nOne sentence. Two sentence.\n' } }));
  await page.route(/\/api\/rewrite\/note$/, (route) => route.fulfill({ json: { claim: 'c', tells: [], advice: 'a' } }));
  await page.getByRole('button', { name: 'Read the page' }).click();
  await expect(page.locator('.rw-title')).toHaveText('Retry design');
  await expect(page.locator('.rw-sent')).toContainText('One sentence.');
  await expect(page.locator('.rw-prog')).toContainText('0 / 2');
});
