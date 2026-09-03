// Multiple clients, resize, and session lifecycle against one server.
import { test, expect } from '../helpers/fixtures';
import { ask, boot, say, termText, waitForTermText } from '../helpers/term';

// v0 semantics: all web sessions share the ONE loop and its event
// broadcast. A reply triggered from page A is also rendered on page B
// (B gets the assistant event without the local user block).
test('two pages, one server: loop events broadcast to every session (v0 shared-session)', async ({ launchBough, browser }) => {
  const b = await launchBough();
  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();
  try {
    await boot(pageA, b.url);
    await boot(pageB, b.url);

    await say(pageA, 'hello from A');
    await waitForTermText(pageA, 'echo: hello from A');
    // The reply reaches B too — shared event stream.
    await waitForTermText(pageB, 'echo: hello from A');
    // But the user block is local to the page that typed it.
    expect(await termText(pageB)).not.toContain('❯ hello from A');
  } finally {
    await ctxA.close();
    await ctxB.close();
  }
});

test('resizing the browser reflows the terminal and the session keeps working', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'before resize', 'echo: before resize');

  const before = await page.evaluate(() => (window as any).sipTerm.term.cols);
  await page.setViewportSize({ width: 700, height: 500 });
  await page.waitForFunction(
    (c: number) => (window as any).sipTerm.term.cols !== c,
    before,
    { timeout: 10_000 },
  );
  const after = await page.evaluate(() => (window as any).sipTerm.term.cols);
  expect(after).toBeLessThan(before);

  // The transcript re-rendered at the new width and the loop still answers.
  await waitForTermText(page, 'echo: before resize');
  await ask(page, 'after resize', 'echo: after resize');
});

test('ctrl+c ends one web session; the server and other sessions survive', async ({ launchBough, browser, request }) => {
  const b = await launchBough();
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  try {
    await boot(page, b.url);
    await ask(page, 'about to quit', 'echo: about to quit');

    await page.click('#terminal');
    // ctrl+c on an idle session arms a two-press quit (1.5 s window);
    // the second press quits the bubbletea program for THIS session,
    // and sip reports the session end and drops the connection.
    await page.keyboard.press('Control+c');
    await waitForTermText(page, 'press ctrl+c again to quit');
    await page.keyboard.press('Control+c');
    await page.waitForFunction(
      () => !(window as any).sipTerm.connected,
      null,
      { timeout: 10_000 },
    );

    // The server is still healthy and a fresh session works.
    const res = await request.get(`${b.url}/health`);
    expect(res.status()).toBe(200);
    const page2 = await ctx.newPage();
    await boot(page2, b.url);
    await ask(page2, 'still alive', 'echo: still alive');
  } finally {
    await ctx.close();
  }
});
