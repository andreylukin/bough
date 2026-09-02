// Config hot-reload: the launcher watches the config file's directory
// and reconciles the row tree; --set overrides are re-applied.
import * as fs from 'fs';
import { test, expect } from '../helpers/fixtures';
import { ask, boot } from '../helpers/term';

function waitForOutput(b: { output(): string }, substr: string, timeoutMs = 8_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const tick = () => {
      if (b.output().includes(substr)) return resolve();
      if (Date.now() > deadline)
        return reject(new Error(`"${substr}" not in bough output within ${timeoutMs}ms:\n${b.output()}`));
      setTimeout(tick, 100);
    };
    tick();
  });
}

test('editing the config mid-session reconciles; the next prompt still answers', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'before reload', 'echo: before reload');

  // A real row change: give the skills row a config key. The watcher
  // debounces 300ms, reconciles, and logs the reload.
  const yml = fs.readFileSync(b.configPath, 'utf8');
  fs.writeFileSync(b.configPath, yml.replace('- id: skills\n  plugin: skills', '- id: skills\n  plugin: skills\n  config:\n    marker: reloaded'));
  await waitForOutput(b, 'bough: reloaded');

  await ask(page, 'after reload', 'echo: after reload');
});

test('a broken config keeps the last good tree; the session keeps working', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'still good', 'echo: still good');

  fs.writeFileSync(b.configPath, 'this is: [not valid yaml\n  nope');
  await waitForOutput(b, 'keeping current tree');

  await ask(page, 'after breakage', 'echo: after breakage');
});
