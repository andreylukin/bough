// Wave-1 CC-parity plugins in real Chrome: an ask block renders and a
// click on an option row answers it; /todo renders checkbox lines; a
// spawned subagent's sub:* blocks land in the transcript.
import { test, expect } from '../helpers/fixtures';
import { boot, clickCell, findRow, say, termText, waitForTermText } from '../helpers/term';

// Asks once via tools.ask, then reflects the tool output (the answer).
const askProvider = `
bough.provider("asker", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("[tool output]") >= 0) return "\\u0060\\u0060\\u0060stop\\nfinal answer: " + last + "\\n\\u0060\\u0060\\u0060";
  return "\\u0060\\u0060\\u0060js\\nconsole.log(tools.ask('fav color?', 'red', 'blue'))\\n\\u0060\\u0060\\u0060";
});
bough.setup({ provider: { default: "asker" } });
`;

// Plays parent and child, keyed on the system prompt: the child (the
// workers subagent prompt) bashes 'echo from-child'; the parent spawns
// the child and answers with the spawn result.
const spawnerProvider = `
var fence = "\\u0060\\u0060\\u0060";
bough.provider("spawner", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (system.indexOf("bough subagent") >= 0) {
    if (last.indexOf("[tool output]") >= 0) return "\\u0060\\u0060\\u0060stop\\nCHILD_FINAL " + last + "\\n\\u0060\\u0060\\u0060";
    return fence + "js\\nconsole.log(tools.bash('echo from-child'))\\n" + fence;
  }
  if (last.indexOf("[tool output]") >= 0) return "\\u0060\\u0060\\u0060stop\\nPARENT_FINAL " + last + "\\n\\u0060\\u0060\\u0060";
  return fence + "js\\nconsole.log(tools.spawn('run the echo'))\\n" + fence;
});
bough.setup({ provider: { default: "spawner" } });
`;

test('ask block renders and clicking an option answers it', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': askProvider } });
  await boot(page, b.url);
  await say(page, 'go');

  // Pending ask: accent question + numbered option rows.
  await waitForTermText(page, '? fav color?');
  await waitForTermText(page, '1. red');
  await waitForTermText(page, '2. blue');

  // Click the "2. blue" row: it answers with that option.
  const row = await findRow(page, '2. blue');
  expect(row).toBeGreaterThanOrEqual(0);
  await clickCell(page, row, 4);

  // Answered ask collapses to a one-liner and the answer feeds the model.
  await waitForTermText(page, '→ blue');
  await waitForTermText(page, 'final answer:');
  expect(await termText(page)).toContain('blue');
});

test('/todo add + list render checkbox lines; done flips the box', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);

  await say(page, '/todo add buy milk');
  await waitForTermText(page, '[ ] 1 buy milk');
  await say(page, '/todo add wash car');
  await waitForTermText(page, '[ ] 2 wash car');

  await say(page, '/todo done 1');
  await waitForTermText(page, '[x] 1 buy milk');

  // Bare /todo re-renders the list.
  await say(page, '/todo');
  const screen = await termText(page);
  expect(screen).toContain('[x] 1 buy milk');
  expect(screen).toContain('[ ] 2 wash car');
});

test('a spawn renders sub: blocks and the parent reply carries the child output', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': spawnerProvider } });
  await boot(page, b.url);
  await say(page, 'go');

  // One card per spawn, updated in place: task, call count, and a
  // ✔ once the child is done.
  await waitForTermText(page, 'sub 1 · run the echo');
  await waitForTermText(page, '✔ sub 1');

  // The child's answer re-enters the parent turn as the spawn result,
  // under its provenance line.
  await waitForTermText(page, '[subagent 1 · task: run the echo]');
  await waitForTermText(page, 'CHILD_FINAL');
  await waitForTermText(page, 'from-child');
  await waitForTermText(page, 'PARENT_FINAL');
});
