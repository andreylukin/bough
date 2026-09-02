// hooks-js: .bough/hooks/<event>/*.js bodies run in the codemode VM.
import { test, expect } from '../helpers/fixtures';
import { ask, boot, say, termText, waitForTermText } from '../helpers/term';

test('a project pre-code-exec deny hook surfaces [hook denied', async ({ launchBough, page }) => {
  const b = await launchBough({
    cwd: { '.bough/hooks/pre-code-exec/deny.js': 'return {deny: "nope-project"}' },
  });
  await boot(page, b.url);
  await say(page, 'CODE!');
  await waitForTermText(page, '[hook denied: nope-project]');
  // The denied block never ran: "hi from codemode" may appear on screen
  // only as source text (tools.bash("echo hi from codemode") in the
  // assistant reply), never as standalone bash OUTPUT on its own row.
  const rows = (await termText(page)).split('\n');
  for (const row of rows.filter((r) => r.includes('hi from codemode'))) {
    expect(row).toContain('tools.bash');
  }
});

test('a global (HOME) pre-code-exec deny hook also fires', async ({ launchBough, page }) => {
  const b = await launchBough({
    home: { '.bough/hooks/pre-code-exec/deny.js': 'return {deny: "nope-global"}' },
  });
  await boot(page, b.url);
  await say(page, 'CODE!');
  await waitForTermText(page, '[hook denied: nope-global]');
});

test('a user-prompt-submit hook can rewrite the input', async ({ launchBough, page }) => {
  const b = await launchBough({
    cwd: {
      '.bough/hooks/user-prompt-submit/rewrite.js':
        'return {input: event.input + " REWRITTEN_BY_HOOK"}',
    },
  });
  await boot(page, b.url);
  await ask(page, 'original words', 'echo: original words REWRITTEN_BY_HOOK');
});

test('a user-prompt-submit hook can block the prompt', async ({ launchBough, page }) => {
  const b = await launchBough({
    cwd: {
      '.bough/hooks/user-prompt-submit/block.js': 'return {block: "blocked-by-policy-xyz"}',
    },
  });
  await boot(page, b.url);
  await say(page, 'try me');
  await waitForTermText(page, 'blocked-by-policy-xyz');
  const screen = await termText(page);
  expect(screen).toContain('✗'); // rendered as an error block
  expect(screen).not.toContain('echo: try me'); // the llm never saw it
});
