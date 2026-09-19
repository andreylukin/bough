// A project's page in the control room, against a real `bough serve` on
// an isolated HOME: the project IS the directory, the first message is
// what creates the main thread, MEMORY.md is a file that round-trips to
// disk, threads sit under their status group, and a link from when
// projects were labels still lands somewhere.
//
// Only one thing is mocked, and only in one test: minting a main thread
// for real starts a container, and no CI machine has the runtime. That
// test fulfils the message POST and patches the project read exactly the
// way serve answers once a main exists; Go's TestMessageProjectStarts-
// AndFeedsMain covers the creation itself.
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort } from '../helpers/bough';

const MAIN = '2026-09-19T08-00-00-00001';
const ERRED = '2026-09-19T08-10-00-00002';
const IDLE = '2026-09-19T08-20-00-00003';
const RELAY_MAIN = '2026-09-19T08-30-00-00004';

const jsonl = (...entries: unknown[]) => entries.map((e) => JSON.stringify(e)).join('\n') + '\n';
const session = (slug: string, title: string, ...rest: unknown[]) =>
  jsonl(
    { seq: 1, kind: 'meta', data: { cwd: '/w/bough', mode: 'project', project: slug } },
    { seq: 2, kind: 'input', data: { text: title } },
    ...rest,
  );

// Two definitions and four transcripts, written before serve boots so it
// reads them the way it reads a real ~/.bough.
const seed: Record<string, string> = {
  '.bough/projects/orbit/project.yml': '# bough project definition. Lives outside every repo; never committed.\nname: Orbit\nrepos: []\n',
  '.bough/projects/orbit/MEMORY.md': 'The control room talks to serve over /api.\n',
  '.bough/projects/relay/project.yml': '# bough project definition. Lives outside every repo; never committed.\nname: Relay\nrepos: []\n',
  [`.bough/history/${MAIN}.jsonl`]: session('orbit', 'Plan the migration',
    { seq: 3, kind: 'assistant', data: { text: 'Start with the definitions.' } }, { seq: 4, kind: 'done', data: {} }),
  [`.bough/history/${ERRED}.jsonl`]: session('orbit', 'Port the PTY suite',
    { seq: 3, kind: 'error', data: { text: 'exit status 1' } }, { seq: 4, kind: 'done', data: {} }),
  [`.bough/history/${IDLE}.jsonl`]: session('orbit', 'Write the docs',
    { seq: 3, kind: 'assistant', data: { text: 'Done.' } }, { seq: 4, kind: 'done', data: {} }),
  [`.bough/history/${RELAY_MAIN}.jsonl`]: session('relay', 'Set up the relay',
    { seq: 3, kind: 'assistant', data: { text: 'Ready.' } }, { seq: 4, kind: 'done', data: {} }),
  // Version 2: the label table is gone, and orbit already has a main
  // thread. relay has none — its page still asks for a first message.
  '.bough/serve/meta.json': JSON.stringify({
    version: 2,
    sessions: { [MAIN]: {}, [ERRED]: {}, [IDLE]: {}, [RELAY_MAIN]: {} },
    mains: { orbit: MAIN },
  }, null, 2) + '\n',
};

interface Serve { url: string; home: string; token: string }

const test = base.extend<{ serve: Serve }>({
  serve: async ({ request }, use) => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-prj-'));
    for (const [rel, text] of Object.entries(seed)) {
      const p = path.join(home, rel);
      fs.mkdirSync(path.dirname(p), { recursive: true });
      fs.writeFileSync(p, text);
    }
    const addr = `127.0.0.1:${await freePort()}`;
    const child = spawn(boughBin, ['serve', '--run', addr], {
      cwd: home, env: { ...process.env, HOME: home }, stdio: 'ignore',
    });
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

test('a project created through the API is a directory, and its page asks for the first message', async ({ serve, page, request }) => {
  const res = await request.post(serve.url + '/api/projects', {
    headers: { Authorization: `Bearer ${serve.token}`, Origin: serve.url },
    data: { name: 'Ship it' },
  });
  expect(res.status()).toBe(200);
  expect((await res.json()).project.slug).toBe('ship-it');
  // The directory IS the project: nothing else records that it exists.
  expect(fs.existsSync(path.join(serve.home, '.bough', 'projects', 'ship-it', 'project.yml'))).toBe(true);

  await page.goto(serve.url + '/#/projects/ship-it');
  await expect(page.getByText('No main thread yet. Send a message to start one.')).toBeVisible();
  await expect(page.locator('h1.prj-name')).toHaveText('Ship it');
  // No thread, so no group header is drawn at all.
  await expect(page.locator('.prj-group-head')).toHaveCount(0);
});

test('MEMORY.md is saved only on purpose, and is still there after a reload', async ({ serve, page }) => {
  await page.goto(serve.url + '/#/projects/relay');
  const editor = page.locator('textarea[aria-label="MEMORY.md"]');
  await expect(editor).toHaveValue('');
  await expect(editor).toHaveAttribute('placeholder', /^Empty\. Write what every session in this project should know/);
  const save = page.getByRole('button', { name: 'Save', exact: true });
  // Nothing typed: there is nothing to write, and nothing writes it but you.
  await expect(save).toBeDisabled();
  await expect(page.getByText('Prepended to every session in this project. Nothing writes this but you and the agent.')).toBeVisible();

  await editor.fill('The relay speaks to agent-browser on the host.\nPorts are forwarded, not proxied.\n');
  await expect(save).toBeEnabled();
  await save.click();
  await expect(page.getByText('MEMORY.md saved.')).toBeVisible();
  await expect(page.locator('.file-meta')).toContainText('2 lines');

  // It landed in the definition directory, which is where every session
  // in the project reads it from.
  const file = path.join(serve.home, '.bough', 'projects', 'relay', 'MEMORY.md');
  await expect.poll(() => fs.readFileSync(file, 'utf8')).toContain('agent-browser on the host');

  await page.reload();
  await expect(page.locator('textarea[aria-label="MEMORY.md"]')).toHaveValue(/Ports are forwarded, not proxied\./);
});

test('the first message creates the main thread', async ({ serve, page }) => {
  let posted = '';
  await page.route(/\/api\/projects\/relay\/message$/, (route) => {
    posted = JSON.parse(route.request().postData() ?? '{}').text;
    return route.fulfill({ json: { ok: true, main: RELAY_MAIN } });
  });
  // What serve answers once the project has a main: main names it, and
  // it stops being one of the threads.
  await page.route(/\/api\/projects\/relay$/, async (route) => {
    if (!posted) return route.continue();
    const detail = await (await route.fetch()).json();
    return route.fulfill({
      json: { ...detail, main: RELAY_MAIN, threads: (detail.threads ?? []).filter((t: { id: string }) => t.id !== RELAY_MAIN) },
    });
  });

  await page.goto(serve.url + '/#/projects/relay');
  const box = page.getByRole('textbox', { name: 'Message the project' });
  await expect(box).toBeFocused();
  await box.fill('Take over the migration.');
  await page.getByRole('button', { name: 'Send', exact: true }).click();

  await expect.poll(() => posted).toBe('Take over the migration.');
  // The page stops asking for a first message and shows main's conversation.
  await expect(page.getByText('No main thread yet. Send a message to start one.')).toHaveCount(0);
  await expect(page.locator('.prj-conv')).toContainText('Set up the relay');
});

test('threads sit under their status group, with the main thread pinned above them', async ({ serve, page }) => {
  await page.goto(serve.url + '/#/projects/orbit');
  await expect(page.locator('h1.prj-name')).toHaveText('Orbit');

  // The main thread is the room, not a peer row: pinned, above the groups.
  const main = page.locator('.prj-main-thread');
  await expect(main).toContainText('Main thread');
  await expect(main).toContainText('orbit');
  await expect(page.locator('.prj-conv')).toContainText('Plan the migration');

  // A failed thread is under Error; a finished one is under Idle, which
  // starts collapsed. Scoped to the column: the left sidebar lists every
  // session in the install by the same titles.
  const col = page.locator('.prj-threads');
  const err = col.locator('.prj-group', { hasText: 'Error' });
  await expect(err.locator('.prj-group-label')).toHaveText('Error');
  await expect(err).toContainText('Port the PTY suite');
  await expect(col.getByText('Write the docs')).toHaveCount(0);
  await col.locator('.prj-group-head', { hasText: 'Idle' }).click();
  await expect(col.getByText('Write the docs')).toBeVisible();

  // Opening a thread swaps the conversation and offers the way back.
  await col.getByText('Port the PTY suite').click();
  await expect(page.locator('.prj-conv')).toContainText('exit status 1');
  await page.getByRole('button', { name: '‹ Main thread' }).click();
  await expect(page.locator('.prj-conv')).toContainText('Plan the migration');
});

test('a link from when projects were labels lands on Projects, not on nothing', async ({ serve, page }) => {
  await page.goto(serve.url + '/#/projects/0199c0f1-7f00-7a11-9000-00000000a001/orb');
  await expect.poll(() => page.evaluate(() => window.location.hash)).toBe('#/projects');
  await expect(page.getByText('Page not found')).toHaveCount(0);
  // And the projects that DO exist are on it, by name.
  await expect(page.locator('.proj-body').getByText('Orbit', { exact: true })).toBeVisible();
});
