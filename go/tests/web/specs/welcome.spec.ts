// First run in the control room: an empty server opens on the welcome, a
// key is saved from it, the folder line says whether a session there could
// edit, and a sample prompt starts a session in the checkout. Offline:
// sessions run llm-echo from the fixture HOME's bough.yml, and every
// provider key is stripped from the environment.
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort } from '../helpers/bough';

type Serve = { url: string; home: string; repo: string };

const test = base.extend<{ serve: Serve }>({
  serve: async ({ request }, use) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-welcome-'));
    const home = path.join(root, 'home');
    const repo = path.join(root, 'repo');
    fs.mkdirSync(path.join(repo, '.git'), { recursive: true });
    fs.mkdirSync(path.join(home, '.bough'), { recursive: true });
    fs.writeFileSync(path.join(home, '.bough', 'bough.yml'), '- id: llm\n  plugin: llm-echo\n');
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home };
    for (const k of ['ANTHROPIC_API_KEY', 'OPENAI_API_KEY', 'OPENROUTER_API_KEY', 'CEREBRAS_API_KEY']) delete env[k];
    const addr = `127.0.0.1:${await freePort()}`;
    const child = spawn(boughBin, ['serve', '--run', addr], { cwd: repo, env, stdio: 'ignore' });
    try {
      // GET / hands out the token cookie the API then requires.
      await expect.poll(async () => {
        try {
          await request.get(`http://${addr}/`);
          return (await request.get(`http://${addr}/api/health`)).status();
        } catch { return 0; }
      }).toBe(200);
      await use({ url: `http://${addr}`, home, repo });
    } finally {
      if (child.exitCode === null && child.signalCode === null) {
        await new Promise<void>((resolve) => {
          const timer = setTimeout(() => child.kill('SIGKILL'), 3000);
          child.once('exit', () => { clearTimeout(timer); resolve(); });
          child.kill('SIGTERM');
        });
      }
      fs.rmSync(root, { recursive: true, force: true });
    }
  },
});

test('an empty server walks from no key to a session that can edit the checkout', async ({ page, serve }) => {
  // The welcome asks the provider whether a saved key works; answer as an
  // offline check would ("unknown" still counts as usable) so the spec
  // never reaches api.anthropic.com.
  await page.route('**/api/setup/check?*', (route) => route.fulfill({ json: { state: 'unknown' } }));
  await page.goto(serve.url);
  await expect(page.getByRole('heading', { name: 'Welcome to bough' })).toBeVisible();
  // Serve ran in the checkout, so the folder step is already done and
  // folds to its path; asking for something stays closed until a key works.
  const folderStep = page.getByRole('listitem').filter({ has: page.getByRole('heading', { name: /Pick a folder/ }) });
  await expect(folderStep).toContainText(serve.repo);
  await expect(page.getByText('1 of 3 done')).toBeVisible();
  const chip = page.getByRole('button', { name: 'Explain this repo' });
  await expect(chip).toHaveCount(0);

  await page.getByLabel('Anthropic API key').fill('sk-ant-test');
  await page.getByRole('button', { name: 'Save key' }).click();
  await expect(page.getByText('2 of 3 done')).toBeVisible();
  expect(fs.readFileSync(path.join(serve.home, '.bough', 'env'), 'utf8')).toContain('ANTHROPIC_API_KEY=sk-ant-test');
  await expect(chip).toBeEnabled();

  await folderStep.getByRole('button', { name: 'Change' }).click();
  const folderLine = page.getByRole('status').filter({ hasText: /checkout|folder/i });
  const folder = page.getByLabel('Folder');
  await folder.fill(serve.home);
  await expect(folderLine).toContainText('Not a git checkout');
  await folder.fill(path.join(serve.home, 'nope'));
  await expect(folderLine).toContainText('No folder at this path');
  await folder.fill(path.join(serve.repo, 'sub-that-is-missing'));
  await expect(folderLine).toContainText('No folder at this path');
  await folder.fill(serve.repo);
  await expect(folderLine).toContainText('Git checkout found');

  await expect(chip).toBeEnabled();
  await chip.click();
  await expect(page.getByText('echo: Explain this repo')).toBeVisible({ timeout: 20_000 });
  await expect(page.getByText('Edits repo', { exact: true })).toBeVisible();

  await page.reload();
  await expect(page.getByRole('heading', { name: 'Welcome to bough' })).toHaveCount(0);
});

test('skipping the welcome sticks across reloads, and the palette brings it back', async ({ page, serve }) => {
  await page.goto(serve.url);
  await page.getByRole('button', { name: 'Skip the welcome' }).click();
  await expect(page.getByText('Nothing needs your attention')).toBeVisible();
  await page.reload();
  await expect(page.getByText('Nothing needs your attention')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Welcome to bough' })).toHaveCount(0);

  // Past the welcome, New still starts in the repo serve was run from, not
  // a read-only home.
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+k' : 'Control+k');
  await expect(page.getByRole('option', { name: /New session in repo.*can edit/ })).toBeVisible();
  await page.getByRole('combobox').fill('welcome');
  await page.getByRole('option', { name: /Show the welcome/ }).click();
  await expect(page.getByRole('heading', { name: 'Welcome to bough' })).toBeVisible();
});

test('a phone opens an empty server on the welcome, not the empty list', async ({ page, serve }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(serve.url);
  await expect(page.getByRole('heading', { name: 'Welcome to bough' })).toBeVisible();
});
