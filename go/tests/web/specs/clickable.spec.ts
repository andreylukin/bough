// Clickable transcript blocks in real Chrome: header clicks toggle
// collapse, wheel scrolling coexists with mouse reporting, the
// inspector click-expands entries, and hit-testing survives a browser
// resize. Cell coordinates are computed from xterm's own metrics
// (helpers/term.ts clickCell).
import { test, expect } from '../helpers/fixtures';
import {
  boot,
  clickCell,
  findRow,
  say,
  termText,
  waitForTermText,
  ask,
} from '../helpers/term';

// Provider that answers RUNIT with a 30-line result and anything else
// with a plain reply (same shape as codemode.spec's longres).
const longres = `
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

test('clicking a result header in Chrome toggles it', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': longres } });
  await boot(page, b.url);
  await say(page, 'RUNIT');
  await waitForTermText(page, '▸ result (30 lines)');

  const row = await findRow(page, '▸ result (30 lines)');
  expect(row).toBeGreaterThanOrEqual(0);
  await clickCell(page, row, 2);
  await waitForTermText(page, 'RESLINE_30_X'); // expanded, pinned to bottom

  // A click inside the expanded body collapses it again.
  const bodyRow = await findRow(page, 'RESLINE_30_X');
  expect(bodyRow).toBeGreaterThanOrEqual(0);
  await clickCell(page, bodyRow, 4);
  await waitForTermText(page, '▸ result (30 lines)');
  expect(await termText(page)).not.toContain('RESLINE_30_X');
});

test('large output is collapsed by default with count and preview', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': longres } });
  await boot(page, b.url);
  await say(page, 'RUNIT');
  await waitForTermText(page, '▸ result (30 lines)');
  const screen = await termText(page);
  expect(screen).toContain('RESLINE_1_X'); // preview on the header line
  expect(screen).not.toContain('RESLINE_2_X'); // body stays hidden
});

test('wheel scroll still works with mouse reporting on', async ({ launchBough, page }) => {
  const init = `
bough.provider("longp", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("GIMME") >= 0) {
    var lines = [];
    for (var i = 1; i <= 200; i++) lines.push("LINE_" + i + "_END");
    return lines.join("\\n");
  }
  return "plain: " + last;
});
bough.setup({ provider: { default: "longp" } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await say(page, 'GIMME');
  await waitForTermText(page, 'LINE_200_END');
  expect(await termText(page)).not.toContain('LINE_50_END'); // bottom-pinned

  // Wheel up over the terminal scrolls the transcript back.
  const box = await page.locator('#terminal').boundingBox();
  if (!box) throw new Error('#terminal has no bounding box');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  let sawEarlier = false;
  for (let i = 0; i < 40 && !sawEarlier; i++) {
    await page.mouse.wheel(0, -120);
    await page.waitForTimeout(30);
    sawEarlier = (await termText(page)).includes('LINE_50_END');
  }
  expect(sawEarlier).toBe(true);

  // And wheel down returns toward the bottom.
  let sawBottom = false;
  for (let i = 0; i < 80 && !sawBottom; i++) {
    await page.mouse.wheel(0, 120);
    await page.waitForTimeout(30);
    sawBottom = (await termText(page)).includes('LINE_200_END');
  }
  expect(sawBottom).toBe(true);
});

test('inspector entry click-expands its JSON', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'inspect me', 'echo: inspect me');

  await page.keyboard.press('Control+o');
  await waitForTermText(page, 'inspecting');
  const row = await findRow(page, 'input');
  expect(row).toBeGreaterThanOrEqual(0);
  await clickCell(page, row, 10);
  await waitForTermText(page, '"seq"'); // inline pretty JSON opened
  expect(await termText(page)).toContain('"kind"');

  // Clicking the same entry closes the JSON again.
  const row2 = await findRow(page, 'input');
  await clickCell(page, row2, 10);
  await page.waitForFunction(() => {
    const buf = (window as any).sipTerm.term.buffer.active;
    for (let i = 0; i < buf.length; i++) {
      if (buf.getLine(i).translateToString(true).includes('"seq"')) return false;
    }
    return true;
  }, null, { timeout: 5_000 });
});

test('click toggling still correct after a browser resize', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': longres } });
  await boot(page, b.url);
  await say(page, 'RUNIT');
  await waitForTermText(page, '▸ result (30 lines)');

  const colsBefore = await page.evaluate(() => (window as any).sipTerm.term.cols);
  await page.setViewportSize({ width: 760, height: 520 });
  await page.waitForFunction(
    (c: number) => (window as any).sipTerm.term.cols !== c,
    colsBefore,
    { timeout: 10_000 },
  );
  await waitForTermText(page, '▸ result (30 lines)');

  const row = await findRow(page, '▸ result (30 lines)');
  expect(row).toBeGreaterThanOrEqual(0);
  await clickCell(page, row, 2);
  // Expanding keeps the header on screen; at this height the 30-line
  // body runs past the bottom, so the tail is one page down.
  await waitForTermText(page, '▾ result (30 lines)');
  await waitForTermText(page, 'RESLINE_1_X');
  await page.keyboard.press('PageDown');
  await waitForTermText(page, 'RESLINE_30_X');
});
