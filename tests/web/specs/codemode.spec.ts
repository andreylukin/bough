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

// A long tool result (> 12 lines) starts collapsed with a "... N more
// lines" tail; the collapse_toggle key (default: tab) expands it.
test('long result collapses and tab expands it', async ({ launchBough, page }) => {
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
  await waitForTermText(page, 'more lines');
  let screen = await termText(page);
  expect(screen).toContain('RESLINE_1_X'); // head shown
  expect(screen).not.toContain('RESLINE_30_X'); // tail hidden while collapsed
  await page.keyboard.press('Tab');
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
