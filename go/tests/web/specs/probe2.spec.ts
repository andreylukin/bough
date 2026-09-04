import { test } from '../helpers/fixtures';
import { boot, say, termText } from '../helpers/term';

test('probe2: rows after RUNIT + tab + enter', async ({ launchBough, page }) => {
  const init = `
bough.provider("longres", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("[tool output]") >= 0) {
    var BT = String.fromCharCode(96);
    return BT+BT+BT+"stop\\nfinished::"+messages.length+"\\n"+BT+BT+BT;
  }
  if (last.indexOf("RUNIT") >= 0) {
    return BT+BT+BT+"js\\nfor (var i = 1; i <= 30; i++) console.log('RESLINE_' + i + '_X')\\n"+BT+BT+BT;
  }
  return "plain: " + last;
});
bough.setup({ provider: { default: "longres" } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await say(page, 'RUNIT');
  await page.waitForTimeout(12000);
  const dump = await termText(page);
  console.log('SCREEN:\n' + dump.split('\n').map((r, i) => i + ':[' + r.trim() + ']').join('\n'));
  await page.keyboard.press('Shift+Tab');
  await page.waitForTimeout(400);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(600);
  const dump2 = await termText(page);
  console.log('AFTER:\n' + dump2.split('\n').map((r, i) => i + ':[' + r.trim() + ']').join('\n'));
});
