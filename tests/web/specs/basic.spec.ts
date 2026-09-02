// Page load, prompt/reply round trips, reload behavior.
import { test, expect } from '../helpers/fixtures';
import { ask, boot, termText, waitForTermText } from '../helpers/term';

test('page loads and the terminal client attaches', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  const info = await page.evaluate(() => ({
    connected: (window as any).sipTerm.connected,
    cols: (window as any).sipTerm.term.cols,
    rows: (window as any).sipTerm.term.rows,
    hasContainer: !!document.querySelector('#terminal'),
  }));
  expect(info.connected).toBe(true);
  expect(info.cols).toBeGreaterThan(0);
  expect(info.rows).toBeGreaterThan(0);
  expect(info.hasContainer).toBe(true);
  // The bough composer prompt is on screen.
  await waitForTermText(page, '>');
});

test('health endpoint answers 200', async ({ launchBough, request }) => {
  const b = await launchBough();
  const res = await request.get(`${b.url}/health`);
  expect(res.status()).toBe(200);
});

test('a prompt gets the echo reply', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'hello there', 'echo: hello there');
  const screen = await termText(page);
  expect(screen).toContain('❯ hello there'); // ❯ user block
  expect(screen).toContain('echo: hello there');
});

test('multi-turn conversation keeps all turns on the transcript', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'first turn', 'echo: first turn');
  await ask(page, 'second turn', 'echo: second turn');
  await ask(page, 'third turn', 'echo: third turn');
  const screen = await termText(page);
  expect(screen).toContain('echo: first turn');
  expect(screen).toContain('echo: second turn');
  expect(screen).toContain('echo: third turn');
});

test('server survives a page reload; the transcript replays from history', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'before reload', 'echo: before reload');

  await page.reload();
  await page.waitForFunction(() => (window as any).sipTerm?.connected, null, { timeout: 15_000 });

  // Each web connection builds a fresh bubbletea model, which replays
  // the session's history — the old transcript IS on the new page.
  await waitForTermText(page, 'echo: before reload');

  // And the server and loop are alive: a new prompt still answers.
  await ask(page, 'after reload', 'echo: after reload');
});
