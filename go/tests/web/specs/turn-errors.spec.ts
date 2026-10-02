import type { Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { test, expect } from '../helpers/serve';

const at = '2026-10-01T12:00:00Z';
const call = (seq: number, cmd: string, exit: number) => ({ seq, at, kind: 'call', text: cmd,
  data: { id: `call-${seq}`, tool: 'bash', cmd, exit, output: exit ? 'No matching example was found.' : 'Example loaded.', ...(exit ? { error: `exit status ${exit}` } : {}) } });
const fail = call(2, 'grep missing example.txt', 1);
const ok = call(3, 'cat example.txt', 0);
const reply = { seq: 4, at, kind: 'assistant', text: 'The example is ready to review. The failed search remains in the command history.' };
const hook = { seq: 5, at, kind: 'hook', text: '', data: { event: 'stop', name: 'summary.js',
  error: 'Error: report unavailable\n    at finish (summary.js:9:2)', input: { event: 'stop' }, output: null } };
const done = (exit?: number) => ({ seq: 6, at, kind: 'done', text: '', data: exit === undefined ? {} : { exit } });

async function session(page: Page, entries: unknown[], status = 'done') {
  const row = { id: 'error-example', title: 'Review an example', cwd: '/w/example', status, live: false, archived: false,
    entries: entries.length + 1, modified: at, lastAt: at, mode: 'local' };
  await page.route(/\/api\/sessions(\?.*)?$/, route => route.fulfill({ json: { sessions: [row] } }));
  await page.route(/\/api\/sessions\/error-example(\?.*)?$/, route => route.fulfill({ json: { session: row,
    entries: [{ seq: 1, at, kind: 'input', text: 'Check the example.' }, ...entries] } }));
  await page.route(/\/api\/sessions\/error-example\/events$/, route => route.abort());
  await page.route(/\/api\/sessions\/error-example\/children$/, route => route.fulfill({ json: { children: [] } }));
}

for (const [width, height] of [[1100, 800], [390, 844]] as const) {
  test(`hook and command errors remain readable and keyboard-accessible at ${width}px`, async ({ sharedServe, page }, info) => {
    await page.setViewportSize({ width, height });
    await session(page, [fail, ok, reply, hook, done(0)]);
    await page.goto(sharedServe.url + '/#/s/error-example');
    const foot = page.locator('.turn-foot');
    await expect(foot.locator('.turn-outcome')).toHaveText('Completed');
    await expect(foot).not.toContainText('Done with a failed command');
    await expect(page.locator('.head-failed')).toHaveCount(0);
    const card = page.getByRole('region', { name: 'Stop hook failed', exact: true });
    await expect(card).toBeVisible();
    await expect(card.locator('.err-msg')).toHaveText('Error: report unavailable');
    await expect(card.getByRole('button', { name: /Retry|Switch model/ })).toHaveCount(0);
    const raw = card.locator('.hook-error-details');
    const toggle = raw.locator(':scope > summary');
    await expect(raw).not.toHaveAttribute('open');
    await toggle.focus();
    await toggle.press('Enter');
    await expect(raw).toHaveAttribute('open');
    await expect(raw.locator(':scope > pre')).toContainText('summary.js:9:2');
    await expect(raw.getByRole('region', { name: 'Input', exact: true })).toContainText('stop');
    await toggle.press('Space');
    await expect(raw).not.toHaveAttribute('open');
    await toggle.press('Enter');
    await expect(raw).toHaveAttribute('open');
    await info.attach(`hook-details-${width}`, { body: await card.screenshot(), contentType: 'image/png' });
    await toggle.press('Enter');

    const issues = foot.locator('.turn-issues');
    const summary = issues.locator(':scope > summary');
    await expect(summary).toHaveText('1 command failed');
    await summary.focus();
    await summary.press('Enter');
    await expect(issues).toHaveAttribute('open');
    const jump = issues.getByRole('button', { name: 'grep missing example.txt', exact: true });
    await jump.focus();
    await jump.press('Enter');
    const command = page.locator('details.block[data-seq="2"]');
    await expect(command).toHaveAttribute('open');
    await expect(command.locator(':scope > summary')).toBeFocused();
    await expect(command).toContainText('exit 1');
    await expect(command).toContainText('No matching example was found.');
    await summary.click();
    await expect(issues).not.toHaveAttribute('open');
    await card.scrollIntoViewIfNeeded();
    await info.attach(`turn-errors-${width}`, { body: await page.screenshot(), contentType: 'image/png' });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const accessibility = await new AxeBuilder({ page }).include('.hook-error').include('.turn-issues').analyze();
    expect(accessibility.violations).toEqual([]);
    if (width < 720) {
      expect((await toggle.boundingBox())!.height).toBeGreaterThanOrEqual(44);
      expect((await summary.boundingBox())!.height).toBeGreaterThanOrEqual(44);
    }
  });
}

test('untraced exit, fatal provider error and cancellation remain distinct', async ({ sharedServe, page }) => {
  await session(page, [reply, hook, done(2)]);
  await page.goto(sharedServe.url + '/#/s/error-example');
  await expect(page.locator('.turn-issues > summary')).toHaveText('Exit 2 recorded');
  await page.locator('.turn-issues > summary').click();
  await expect(page.locator('.turn-issues-body')).toContainText('without a matching command result');
  await expect(page.locator('.turn-foot')).not.toContainText('command failed');

  await page.unrouteAll({ behavior: 'wait' });
  await session(page, [{ seq: 2, at, kind: 'error', text: '401 Unauthorized' }, done()], 'error');
  await page.reload();
  await expect(page.locator('.err-card')).toBeVisible();
  await expect(page.locator('.turn-foot')).not.toContainText('Completed');
  await expect(page.locator('.thread-head')).toContainText('Failed');

  await page.unrouteAll({ behavior: 'wait' });
  await session(page, [{ ...fail, data: { ...fail.data, exit: 130, canceled: true } },
    { seq: 5, at, kind: 'cancelled', text: '' }, done(130)], 'stopped');
  await page.reload();
  await expect(page.locator('.turn-outcome')).toHaveText('Stopped');
  await expect(page.locator('.turn-issues')).toHaveCount(0);
  await expect(page.locator('.turn-foot')).not.toContainText('Completed');
});

for (const format of ['native', 'legacy'] as const) {
  test(`${format} failure disclosure opens its own collapsed command, not a same-seq row in another turn`, async ({ sharedServe, page }) => {
    const program = 'console.log(tools.bash("grep missing example.txt"))';
    const succeeded = 'console.log(tools.bash("cat example.txt"))';
    const failedSeq = format === 'native' ? 2 : 3;
    const entries = format === 'native' ? [fail, ok, reply, done(0)] : [
      { seq: 2, at, kind: 'code', text: program },
      { seq: 3, at, kind: 'result', text: 'No matching example was found.', data: { code: program, exit: 1 } },
      { seq: 4, at, kind: 'code', text: succeeded },
      { seq: 5, at, kind: 'result', text: 'Example loaded.', data: { code: succeeded, exit: 0 } },
      { ...reply, seq: 6 }, { ...done(0), seq: 7 },
    ];
    await session(page, entries);
    await page.goto(sharedServe.url + '/#/s/error-example');
    const actual = page.locator('.turn');
    await expect(actual.locator('.turn-issues > summary')).toHaveText('1 command failed');
    const target = actual.locator(`details.block[data-seq="${failedSeq}"]`);
    await expect(target).not.toHaveAttribute('open');
    // A second rendered turn can contain a reused sequence; opening this
    // disclosure must stay scoped to its own turn, not document.querySelector.
    await actual.evaluate((element, seq) => {
      const other = document.createElement('section');
      other.className = 'turn';
      other.dataset.decoy = 'true';
      const details = document.createElement('details');
      details.className = 'block';
      details.dataset.seq = String(seq);
      const summary = document.createElement('summary');
      summary.textContent = 'Different turn';
      details.append(summary);
      other.append(details);
      element.before(other);
    }, failedSeq);
    const own = page.locator('.turn:not([data-decoy])');
    const summary = own.locator('.turn-issues > summary');
    await summary.focus();
    await summary.press('Enter');
    await own.locator('.turn-issues-body button').click();
    const opened = own.locator(`details.block[data-seq="${failedSeq}"]`);
    await expect(opened).toHaveAttribute('open');
    await expect(opened.locator(':scope > summary')).toBeFocused();
    await expect(opened.locator('.block-body')).toBeVisible();
    await expect(page.locator('[data-decoy] details')).not.toHaveAttribute('open');
  });
}
