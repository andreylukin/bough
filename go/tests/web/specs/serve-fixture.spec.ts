// The two ways a spec gets a `bough serve` from helpers/serve.ts.
//
// `serve` is one process per test: seed the HOME, override bough.yml,
// and nothing another test does can reach it. `sharedServe` is one
// process per worker: the boot is paid once, and a test stays out of
// its neighbours' way by working only in the session it created.
import * as fs from 'fs';
import * as path from 'path';
import { test, expect } from '../helpers/serve';

test.describe('a serve of its own', () => {
  test.use({
    serveOpts: {
      home: { '.bough/projects/orbit/project.yml': 'name: Orbit\nrepos: []\n' },
      config: '- id: llm\n  plugin: llm-echo\n  config:\n    model: fixture-model\n',
    },
  });

  test('seeds the HOME, reads the overridden bough.yml, and opens the page signed in', async ({ serve, page }) => {
    // The override is the config every session resolves, not a file
    // that merely exists: serve names its model back.
    const models = await (await serve.api.get('/api/models')).json();
    expect(models.default).toMatchObject({ plugin: 'llm-echo', model: 'fixture-model' });

    // Signed in before the first navigation, so a deep link needs no
    // detour through "/" for the cookie.
    const cookies = await page.context().cookies(serve.url);
    expect(cookies.find((c) => c.name === 'bough_serve_token')?.value).toBe(serve.token);
    await page.goto(serve.url + '/#/projects');
    await expect(page.locator('.proj-body').getByText('Orbit', { exact: true })).toBeVisible();
    expect(fs.existsSync(path.join(serve.home, '.bough', 'projects', 'orbit', 'project.yml'))).toBe(true);
  });
});

test.describe('one serve per worker', () => {
  // In order, in one worker, so the second test can see it met the
  // same process the first did.
  test.describe.configure({ mode: 'default' });
  let firstPid = 0;

  for (const word of ['alpha', 'beta']) {
    test(`a test works only in the session it created (${word})`, async ({ sharedServe, page }) => {
      if (firstPid === 0) firstPid = sharedServe.pid;
      expect(sharedServe.pid).toBe(firstPid);

      const id = await sharedServe.newSession(`say ${word}`);
      await page.goto(`${sharedServe.url}/#/s/${id}`);
      await expect(page.locator('.thread')).toContainText(`echo: say ${word}`);
    });
  }
});
