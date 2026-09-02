// Codemode flows: js execution, result collapse, long-output scrolling.
import { test, expect } from '../helpers/fixtures';
import { boot, say, termText, waitForTermText } from '../helpers/term';

test('CODE! runs the js block and shows code, result, and done separator', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await say(page, 'CODE!');
  // The code box carries the js tag and the tool call.
  await waitForTermText(page, 'tools.bash');
  // The bash tool actually ran.
  await waitForTermText(page, 'hi from codemode');
  // The turn finishes: echo answers the fed-back tool output, then done.
  await waitForTermText(page, 'echo: [tool output]');
  await waitForTermText(page, '────────');
  const screen = await termText(page);
  expect(screen).toContain('js');
  expect(screen).toContain('result');
});

// A long tool result (> 3 lines) starts collapsed to a "▸ result
// (N lines): <preview>" header; tab (block_next) focuses it and enter
// (collapse_toggle with an empty composer) expands it.
test('long result collapses; tab + enter expands it', async ({ launchBough, page }) => {
  const init = `
bough.provider("longres", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("[tool output]") >= 0) return "finished::" + messages.length;
  if (last.indexOf("RUNIT") >= 0) {
    return "\\u0060\\u0060\\u0060js\\nfor (var i = 1; i <= 30; i++) console.log('RESLINE_' + i + '_X')\\n\\u0060\\u0060\\u0060";
  }
  return "plain: " + last;
});
bough.setup({ provider: { default: "longres" } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await say(page, 'RUNIT');
  await waitForTermText(page, '▸ result (30 lines)');
  let screen = await termText(page);
  expect(screen).toContain('RESLINE_1_X'); // header preview shows the first line
  expect(screen).not.toContain('RESLINE_30_X'); // body hidden while collapsed
  await page.keyboard.press('Shift+Tab'); // block_prev: focus the newest block (the result)
  await page.keyboard.press('Enter'); // collapse_toggle on the focused block
  await waitForTermText(page, 'RESLINE_30_X');
});

// A very long reply pins the transcript to the bottom; scroll keys
// (default: pgup) walk back to the top.
test('long output scrolls: bottom pinned, pgup reaches the start', async ({ launchBough, page }) => {
  const init = `
bough.provider("longp", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("GIMME") >= 0) {
    var lines = [];
    for (var i = 1; i <= 200; i++) lines.push("LINE_" + i + "_END");
    return "\\u0060\\u0060\\u0060text\\n" + lines.join("\\n") + "\\n\\u0060\\u0060\\u0060";
  }
  return "plain: " + last;
});
bough.setup({ provider: { default: "longp" } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await say(page, 'GIMME');
  await waitForTermText(page, 'LINE_200_END');
  expect(await termText(page)).not.toContain('LINE_1_END'); // scrolled past

  // Page up until the first line comes into view.
  let found = false;
  for (let i = 0; i < 20 && !found; i++) {
    await page.keyboard.press('PageUp');
    await page.waitForTimeout(50);
    found = (await termText(page)).includes('LINE_1_END');
  }
  expect(found).toBe(true);
});
