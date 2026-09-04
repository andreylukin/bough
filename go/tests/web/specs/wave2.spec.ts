// Wave-2 UX fixes in real Chrome: the fresh-session welcome text (and
// its removal on the first turn), long-error wrapping with the
// credential hint, and the todo list rendering once per mutation.
import { test, expect } from '../helpers/fixtures';
import { boot, say, termText, waitForTermText } from '../helpers/term';

// Fails every completion with an auth-shaped error, like a dead key.
const authFailProvider = `
bough.provider("authfail", function (system, messages) {
  throw new Error("POST https://api.example.com/v1/messages: 401 Unauthorized: invalid x-api-key: ` +
  'x'.repeat(220) + ` END_OF_ERROR");
});
bough.setup({ provider: { default: "authfail" } });
`;

test('fresh session shows the welcome text; the first turn removes it', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);

  await waitForTermText(page, 'bough — a coding agent');
  await waitForTermText(page, '/ for commands');
  await waitForTermText(page, 'ask me to do something — I act by running code');

  await say(page, 'hello');
  await waitForTermText(page, 'echo: hello');
  expect(await termText(page)).not.toContain('/ for commands');
});

test('a long auth error wraps to the terminal width and carries the credential hint', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': authFailProvider } });
  await boot(page, b.url);
  await say(page, 'go');

  // The tail of the error is on screen (wrapped, not clipped) …
  await waitForTermText(page, 'END_OF_ERROR');
  // … and the auth shape appends the credential hint.
  await waitForTermText(page, 'ANTHROPIC_API_KEY');
  await waitForTermText(page, '/model');
});

test('a /todo mutation renders the list once', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);

  await say(page, '/todo add solo item');
  await waitForTermText(page, '[ ] 1 solo item');
  const screen = await termText(page);
  const copies = screen.split('[ ] 1 solo item').length - 1;
  expect(copies).toBe(1);
});

// One finished session, seeded straight into the temp HOME (the
// sessions.spec pattern) — resuming it must replay history, not the
// welcome text.
const seededSession = {
  '.bough/history/2026-08-31T10-00-00-54321.jsonl':
    '{"seq":1,"kind":"input","data":{"text":"seeded question"}}\n' +
    '{"seq":2,"kind":"assistant","data":{"text":"echo: seeded question"}}\n' +
    '{"seq":3,"kind":"done","data":{"text":""}}\n',
};

test('reloading with history replays the transcript without the welcome text', async ({ launchBough, page }) => {
  const b = await launchBough({ home: seededSession, args: ['--continue'] });
  await boot(page, b.url);

  // The seeded transcript replays …
  await waitForTermText(page, 'echo: seeded question');
  // … and the fresh-session welcome never shows on a non-empty replay.
  const screen = await termText(page);
  expect(screen).not.toContain('/ for commands');
  expect(screen).not.toContain('bough — a coding agent');
});

test('a long error hard-wraps across rows instead of clipping', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': authFailProvider } });
  await boot(page, b.url);
  await say(page, 'go');
  await waitForTermText(page, 'END_OF_ERROR');

  // The 220-char x-run cannot fit one terminal row: wrapping must have
  // split it across at least two rows (clipping would leave one).
  const screen = await termText(page);
  const cols: number = await page.evaluate(() => (window as any).sipTerm.term.cols);
  const xRows = screen.split('\n').filter((r) => r.includes('xxxxxxxxxx'));
  expect(xRows.length).toBeGreaterThanOrEqual(2);
  for (const r of xRows) expect(r.length).toBeLessThanOrEqual(cols);
});

test('/model shows the provider row and swaps to llm-echo live', async ({ launchBough, page }) => {
  // Start on llm-anthropic (mounts fine without a key — the client is
  // lazy) so the swap to llm-echo is observable in the next reply.
  const b = await launchBough({ sets: ['llm.plugin=llm-anthropic', 'llm.model=claude-test'] });
  await boot(page, b.url);

  // List: both rows, the providers, and the usage line.
  await say(page, '/model list');
  await waitForTermText(page, 'model: llm-anthropic · claude-test');
  await waitForTermText(page, 'llm-openrouter');
  await waitForTermText(page, 'usage: /model');

  // Live swap; the very next turn is answered by llm-echo.
  await say(page, '/model llm-echo');
  await waitForTermText(page, 'model: llm-echo');
  await say(page, 'after swap');
  await waitForTermText(page, 'echo: after swap');
});

test('!cmd runs the shell and never becomes an LLM turn', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);

  await say(page, '!echo bang-marker-42');
  // Command echo, labeled result block, and the output itself.
  await waitForTermText(page, '! echo bang-marker-42');
  await waitForTermText(page, 'bang-marker-42');

  // A follow-up LLM turn works; the bang line never reached the model.
  await say(page, 'ping');
  await waitForTermText(page, 'echo: ping');
  const screen = await termText(page);
  expect(screen).not.toContain('echo: !echo');
});
