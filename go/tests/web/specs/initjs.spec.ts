// init.js: themes, keymaps, JS tools, JS providers, cognition,
// projection — the whole bough.* config surface, observed end-to-end
// through the browser terminal.
import { test, expect } from '../helpers/fixtures';
import { ask, boot, say, termText, typeInTerm, vpText, waitForTermText } from '../helpers/term';

test('a theme change from init.js recolors the user line', async ({ launchBough, page }) => {
  const init = `
bough.setup({ ui: { theme: { user: "#ff0000:bold" } } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await ask(page, 'paint me red', 'echo: paint me red');

  // Find the user line ("❯ paint me red") and read its fg color from
  // the buffer cell attributes (canvas rendering leaves no DOM to read).
  const cell = await page.evaluate(() => {
    const buf = (window as any).sipTerm.term.buffer.active;
    for (let i = 0; i < buf.length; i++) {
      const line = buf.getLine(i);
      const text = line.translateToString(true);
      const col = text.indexOf('❯');
      if (col >= 0 && text.includes('paint me red')) {
        const c = line.getCell(col);
        return { fg: c.getFgColor(), bold: !!(c.isBold && c.isBold()), found: true };
      }
    }
    return { fg: -1, bold: false, found: false };
  });
  expect(cell.found).toBe(true);
  // #ff0000 arrives as truecolor RGB or as its nearest 256/16 palette
  // slot depending on what the PTY advertises; all are "red".
  expect([0xff0000, 196, 9, 1]).toContain(cell.fg);
  expect(cell.bold).toBe(true);
});

test('a keymap change from init.js rebinds clear_input', async ({ launchBough, page }) => {
  const init = `
bough.setup({ ui: { keymap: { clear_input: "ctrl+g" } } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await typeInTerm(page, 'draft text to discard');
  await waitForTermText(page, 'draft text to discard');
  await page.keyboard.press('Control+g');
  await page.waitForFunction(() => {
    const buf = (window as any).sipTerm.term.buffer.active;
    for (let i = 0; i < buf.length; i++) {
      if (buf.getLine(i).translateToString(true).includes('draft text to discard')) return false;
    }
    return true;
  }, null, { timeout: 5_000 });
});

test('a JS tool registered with bough.tool runs from a code block', async ({ launchBough, page }) => {
  const init = `
bough.tool("greet", function () { return "TOOL_SAYS_HI_5150"; });
bough.provider("toolp", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("[tool output]") >= 0) return "tool done";
  return "\\u0060\\u0060\\u0060js\\nconsole.log(tools.greet())\\n\\u0060\\u0060\\u0060";
});
bough.setup({ provider: { default: "toolp" } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await say(page, 'go');
  await waitForTermText(page, 'TOOL_SAYS_HI_5150');
  await waitForTermText(page, 'tool done');
});

test('a JS parrot provider serves the whole conversation', async ({ launchBough, page }) => {
  const init = `
bough.provider("parrot", function (system, messages) {
  var last = messages[messages.length - 1].content;
  return "parrot(" + last + ") after " + messages.length + " msgs";
});
bough.setup({ provider: { default: "parrot" } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await ask(page, 'polly', 'parrot(polly) after 1 msgs');
  await ask(page, 'wants a cracker', 'parrot(wants a cracker) after 3 msgs');
});

test('setup.system.append reaches the system prompt (cognition shorthand)', async ({ launchBough, page }) => {
  const init = `
bough.provider("systail", function (system, messages) {
  return "SYSTAIL::" + system.slice(-80).replace(/\\u0060/g, "'");
});
bough.setup({
  provider: { default: "systail" },
  system: { append: "APPENDED_COGNITION_MARK_4242" },
});
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await ask(page, 'anything', 'APPENDED_COGNITION_MARK_4242');
});

test('a JS projection replaces the model messages', async ({ launchBough, page }) => {
  const init = `
bough.project(function (entries) {
  return [{ role: "user", content: "PROJECTED_INPUT_2718" }];
});
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  // llm-echo sees only what the projection produced.
  await ask(page, 'the real input', 'echo: PROJECTED_INPUT_2718');
  // Viewport-scoped: the projection governs the LOOP's messages, not
  // every call in the process. The session-title plugin names the
  // session from what the user actually typed (naming it
  // "PROJECTED_INPUT_2718" would be absurd), and with no llm-small row
  // that title comes from llm-echo too — so it lands on the status
  // bar, outside the transcript this assertion is about.
  expect(await vpText(page)).not.toContain('echo: the real input');
});

test('an unknown bough.setup key fails the mount loudly', async ({ launchBough }) => {
  await expect(
    launchBough({
      cwd: { '.bough/init.js': 'bough.setup({ bogus: 1 });' },
      readyTimeoutMs: 8_000,
    }),
  ).rejects.toThrow(/unknown key "bogus"|unknown key/);
});
