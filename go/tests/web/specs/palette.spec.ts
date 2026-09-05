// The "/" command palette in the web client: open + filter, arrow +
// Enter dispatch (system block, never the LLM), Tab completion.
import { Page } from '@playwright/test';
import { test, expect } from '../helpers/fixtures';
import { boot, termText, typeInTerm, waitForTermText, expectNotOnScreen } from '../helpers/term';

/** Wait until substr is GONE from the terminal screen. */
async function waitForTermGone(page: Page, substr: string, timeoutMs = 10_000): Promise<void> {
  await page.waitForFunction(
    (m: string) => {
      const b = (window as any).sipTerm.term.buffer.active;
      for (let i = 0; i < b.length; i++) {
        if (b.getLine(i).translateToString(true).includes(m)) return false;
      }
      return true;
    },
    substr,
    { timeout: timeoutMs },
  );
}

test('typing "/" opens the palette and typing filters it', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);

  await typeInTerm(page, '/');
  // The overlay is a window on the commands: name + dimmed summary per
  // row, at most palMaxRows (10) of them. Anchor on the FIRST row —
  // "/clear" sorts first and stays visible however many commands are
  // added. Anchoring on a row that merely happened to fit (/quit's
  // "exit bough") broke the moment two more commands shipped.
  await waitForTermText(page, '/clear');
  let screen = await termText(page);
  expect(screen).toContain('/help');
  expect(screen).toContain('list commands');

  // A command below the fold is found by filtering, not by scrolling.
  await typeInTerm(page, 'ses');
  await waitForTermText(page, 'pick a session to resume');
  for (let i = 0; i < 3; i++) await page.keyboard.press('Backspace');
  await waitForTermText(page, '/clear');

  // "he" narrows to /help alone (prefix tier); the rest drop off.
  await typeInTerm(page, 'he');
  await waitForTermGone(page, '/clear');
  screen = await termText(page);
  expect(screen).toContain('/help');
  expect(screen).toContain('list commands');
  expect(screen).not.toContain('/clear');
  expect(screen).not.toContain('/connect');
});

test('arrows + Enter run /help and the system block renders', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);

  await typeInTerm(page, '/');
  await waitForTermText(page, '/clear');
  // /help sits first; a down then an up walks off it and back, and
  // Enter dispatches the selected row.
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowUp');
  await page.keyboard.press('Enter');

  // The dispatched line echoes as a command block, and the /help table
  // (a "system" block) lists every command with its summary.
  await waitForTermText(page, '❯ /help');
  await waitForTermText(page, 'collapse all blocks');
  const screen = await termText(page);
  expect(screen).toContain('/collapse');
  expect(screen).toContain('/sessions');
  // The line never reached the LLM: no echo reply.
  await expectNotOnScreen(page, 'echo: /help');
});

test('Tab completes the draft to "/help " and leaves the palette open', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);

  await typeInTerm(page, '/he');
  await waitForTermText(page, '/help');
  await page.keyboard.press('Tab');

  // Tab rewrote the composer to "/help " (cursor at end): typing an
  // arg lands after the completed name, not after the old "/he".
  await typeInTerm(page, 'x');
  await waitForTermText(page, '/help x');
  await expectNotOnScreen(page, '/hex');
});
