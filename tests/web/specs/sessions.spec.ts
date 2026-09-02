// Session resume in the browser: --continue replays the most recent
// session's transcript, bare --resume shows the picker first and enter
// loads the selected session.
import * as fs from 'fs';
import * as path from 'path';
import { test, expect } from '../helpers/fixtures';
import { ask, boot, termText, waitForTermText } from '../helpers/term';

// One finished two-turn session, seeded straight into the temp HOME.
const seededSession = {
  '.bough/history/2026-08-31T10-00-00-12345.jsonl':
    '{"seq":1,"kind":"input","data":{"text":"first question"}}\n' +
    '{"seq":2,"kind":"assistant","data":{"text":"echo: first question"}}\n' +
    '{"seq":3,"kind":"done","data":{"text":""}}\n' +
    '{"seq":4,"kind":"input","data":{"text":"second question"}}\n' +
    '{"seq":5,"kind":"assistant","data":{"text":"echo: second question"}}\n' +
    '{"seq":6,"kind":"done","data":{"text":""}}\n',
};

const seededFile = (home: string) =>
  path.join(home, '.bough', 'history', '2026-08-31T10-00-00-12345.jsonl');

test('--continue replays the latest session transcript', async ({ launchBough, page }) => {
  const b = await launchBough({ home: seededSession, args: ['--continue'] });
  await boot(page, b.url);

  // The seeded transcript is on screen without typing anything.
  await waitForTermText(page, 'echo: second question');
  const screen = await termText(page);
  expect(screen).toContain('first question');
  expect(screen).toContain('echo: first question');
  expect(screen).toContain('second question');

  // The resumed session is live and appends to the SAME file.
  const before = fs.readFileSync(seededFile(b.home), 'utf8').trim().split('\n').length;
  await ask(page, 'third question', 'echo: third question');
  const after = fs.readFileSync(seededFile(b.home), 'utf8').trim().split('\n').length;
  expect(after).toBeGreaterThan(before);
});

test('bare --resume shows the session picker; enter loads it', async ({ launchBough, page }) => {
  const b = await launchBough({ home: seededSession, args: ['-r'] });
  await boot(page, b.url);

  // Picker first, not the chat view: header + the session's title row.
  await waitForTermText(page, 'resume a session');
  await waitForTermText(page, 'first question');

  await page.evaluate(() => (window as any).sipTerm.webterm.focus());
  await page.keyboard.press('Enter');

  // The picked session's transcript replays into the chat view.
  await waitForTermText(page, 'echo: second question');
  expect(await termText(page)).toContain('echo: first question');
});
