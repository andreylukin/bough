import { test } from '../helpers/fixtures';
import { boot, say, termText } from '../helpers/term';

test('probe3: projection status bar', async ({ launchBough, page }) => {
  const b = await launchBough({ cwd: { '.bough/init.js': 'bough.project(function (entries) { return [{ role: "user", content: "PROJECTED_INPUT_2718" }]; });' } });
  await boot(page, b.url);
  await say(page, 'the real input');
  await page.waitForTimeout(4000);
  const dump = await termText(page);
  console.log('SCREEN:\n' + dump.split('\n').map((r, i) => i + ':[' + r.trim() + ']').join('\n'));
});
