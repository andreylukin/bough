import { test } from '../helpers/fixtures';
import { boot, say, termText } from '../helpers/term';

test('probe: buffer rows after GIMME', async ({ launchBough, page }) => {
  const init = `
bough.provider("longp", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (last.indexOf("GIMME") >= 0) {
    var lines = [];
    for (var i = 1; i <= 200; i++) lines.push("LINE_" + i + "_END");
    var BT = String.fromCharCode(96);
    return BT+BT+BT + "stop\\n" + lines.join("\\n") + "\\n" + BT+BT+BT;
  }
  return "plain: " + last;
});
bough.setup({ provider: { default: "longp" } });
`;
  const b = await launchBough({ cwd: { '.bough/init.js': init } });
  await boot(page, b.url);
  await say(page, 'GIMME');
  await page.waitForFunction(() => {
    const b = (window as any).sipTerm.term.buffer.active;
    for (let i = 0; i < b.length; i++) {
      if (b.getLine(i).translateToString(true).includes('LINE_200_END')) return true;
    }
    return false;
  }, null, { timeout: 10000 });
  await page.waitForTimeout(1000);
  const info = await page.evaluate(() => {
    const b = (window as any).sipTerm.term.buffer.active;
    const rows: string[] = [];
    rows.push('bufferLength=' + b.length);
    for (let i = 0; i < b.length; i++) {
      rows.push(i + ':[' + b.getLine(i).translateToString(true) + ']');
    }
    return rows.join('\n');
  });
  console.log(info);
});
