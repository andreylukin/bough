import { spawn } from 'child_process';
import { writeFileSync } from 'fs';
import * as path from 'path';
import type { Locator, Page } from '@playwright/test';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort, type Bough } from '../helpers/bough';
import { ask, boot } from '../helpers/term';
type Fire = { name: string; [key: string]: unknown };

// --web is the terminal client; the control room must share its isolated HOME.
const test = base.extend<{ controlRoom: (b: Bough) => Promise<string> }>({
  controlRoom: async ({ request }, use) => {
    const children: ReturnType<typeof spawn>[] = [];
    try {
      await use(async (b) => {
        const url = `http://127.0.0.1:${await freePort()}`;
        const child = spawn(boughBin, ['serve', '--run', url.replace('http://', '')], {
          cwd: b.cwd, env: { ...process.env, HOME: b.home }, stdio: 'ignore',
        });
        children.push(child);
        await expect.poll(async () => {
          try { return (await request.get(url + '/api/health')).status(); }
          catch { return 0; }
        }).toBe(200);
        return url;
      });
    } finally {
      for (const child of children) {
        if (child.exitCode !== null || child.signalCode !== null) continue;
        await new Promise<void>((resolve) => {
          const timer = setTimeout(() => child.kill('SIGKILL'), 3000);
          child.once('exit', () => { clearTimeout(timer); resolve(); });
          child.kill('SIGTERM');
        });
      }
    }
  },
});

function fire(name: string, extra: Partial<Fire> = {}): Fire {
  return { name, event: 'pre-tool-use', session: '', at: '2026-06-01T12:00:00Z',
    ms: 3, decision: '', error: '', notice: '', truncated: [], ...extra };
}

async function mockFires(page: Page, fires: Fire[]) {
  await page.route('**/api/hooks', (route) => route.fulfill({
    json: { hooks: [], watchers: [], rules: [], plugins: [], fires },
  }));
}

async function expectPayload(panel: Locator, side: 'Input' | 'Output', value: unknown) {
  await expect(panel.getByRole('region', { name: side, exact: true }).locator('pre'))
    .toHaveText(JSON.stringify(value, null, 2));
}

test('disk JS capture and current definition open from transcript, recent fire and latest hook', async ({
  launchBough, controlRoom, page,
}) => {
  const rel = '.bough/hooks/user-prompt-submit/inspect.js';
  const description = 'Tag submitted prompts so their rewritten input can be inspected.';
  const source = '// Description: ' + description + '\nreturn {input: event.input + " CAPTURED"}';
  const b = await launchBough({ cwd: { [rel]: source } });
  await boot(page, b.url);
  await ask(page, 'inspect original', 'echo: inspect original CAPTURED');
  const url = await controlRoom(b);
  const data = await (await page.request.get(url + '/api/hooks')).json();
  const recorded = data.fires.find((f: Fire) => f.name === 'inspect.js');
  expect(recorded).toBeTruthy();
  expect(recorded.path).toBe(path.join(b.cwd, rel));
  expect(recorded.description).toBe(description);
  expect(recorded.input.input).toBe('inspect original');
  expect(recorded.output).toEqual({ input: 'inspect original CAPTURED' });

  // Opening an invocation must fetch today's file without changing its capture.
  const currentDescription = 'Tag future prompts with the updated marker.';
  const current = '// Description: ' + currentDescription + '\nreturn {input: event.input + " FUTURE"}';
  writeFileSync(path.join(b.cwd, rel), current);
  await page.goto(url + '/#/s/' + recorded.session);
  const turn = page.locator('.turn-hooks').filter({ hasText: 'inspect' }).first();
  await turn.locator(':scope > summary').click();
  const invocation = turn.locator('.hook-invocation').filter({ hasText: 'inspect.js ·' });
  await invocation.locator(':scope > summary').click();
  await expect(invocation.locator('.hk-description')).toHaveText(description);
  await expectPayload(invocation, 'Input', recorded.input);
  await expectPayload(invocation, 'Output', recorded.output);
  await expect(invocation).toContainText('Current file; may differ from this run');
  await invocation.getByRole('button', { name: 'Open definition', exact: true }).click();
  await expect(invocation.getByRole('textbox', { name: 'File contents' })).toHaveValue(current);

  await page.goto(url + '/#/hooks');
  const recent = page.locator('.hk-fold').filter({ has: page.locator('.hk-name', { hasText: /^inspect\.js$/ }) });
  await recent.locator('.hk-name').click();
  await expect(recent.locator('.hk-description')).toHaveText(description);
  await expectPayload(recent, 'Input', recorded.input);
  await expectPayload(recent, 'Output', recorded.output);
  await recent.getByRole('button', { name: 'Open definition', exact: true }).click();
  await expect(recent.getByRole('textbox', { name: 'File contents' })).toHaveValue(current);

  const hook = page.locator('.hk2-row').filter({ has: page.locator('.hk2-name', { hasText: /^inspect\.js$/ }) });
  await expect(hook.locator(':scope > .hk-description')).toHaveText(currentDescription);
  await hook.locator('summary').filter({ hasText: 'Latest recorded input / output' }).click();
  await expect(hook.locator('.hk-inspect .hk-description')).toHaveText(description);
  await expectPayload(hook, 'Input', recorded.input);
  await expectPayload(hook, 'Output', recorded.output);
  await expect(hook.getByRole('button', { name: 'Open definition', exact: true })).toHaveCount(1);
  await expect(hook.locator('.hk-session')).toHaveAttribute('href', '#/s/' + recorded.session);
});

test('quiet grouped records retain every invocation input and output in newest-first order', async ({
  launchBough, controlRoom, page,
}) => {
  const b = await launchBough();
  const url = await controlRoom(b);
  await mockFires(page, [1, 2, 3].map((n) => fire('quiet', {
    at: `2026-06-01T12:00:0${n}Z`, description: `Purpose recorded for run ${n}`, input: { invocation: n }, output: { result: n },
  })));
  await page.goto(url + '/#/hooks');
  const group = page.locator('.hk-group');
  await expect(group).toHaveCount(1);
  await expect(group.locator(':scope > summary')).toContainText('×3');
  await expect(group.getByRole('region', { name: 'Input', exact: true }).first()).not.toBeVisible();
  await group.locator(':scope > summary').click();
  const records = group.locator('.hk-runs > li');
  await expect(records).toHaveCount(3);
  for (let i = 0; i < 3; i++) {
    await expect(records.nth(i).locator('.hk-description')).toHaveText('Purpose recorded for run ' + (3 - i));
    await expectPayload(records.nth(i), 'Input', { invocation: 3 - i });
    await expectPayload(records.nth(i), 'Output', { result: 3 - i });
  }
});

test('legacy missing capture differs from explicit null and oversize omission', async ({
  launchBough, controlRoom, page,
}) => {
  const b = await launchBough();
  const url = await controlRoom(b);
  await mockFires(page, [
    fire('legacy'),
    fire('null-result', { input: null, output: null, inputBytes: 4, outputBytes: 4 }),
    fire('oversize', { inputBytes: 70000, outputBytes: 80000, inputTruncated: true, outputTruncated: true }),
  ]);
  await page.goto(url + '/#/hooks');
  for (const name of ['legacy', 'null-result', 'oversize']) {
    const row = page.locator('.hk-fold').filter({ has: page.locator('.hk-name', { hasText: new RegExp('^' + name + '$') }) });
    await row.locator(':scope > summary').click();
    for (const side of ['Input', 'Output'] as const) {
      const payload = row.getByRole('region', { name: side, exact: true });
      if (name === 'legacy') {
        await expect(payload).toContainText('Unavailable — this record has no captured ' + side.toLowerCase());
        await expect(payload).not.toContainText('no output returned');
      } else if (name === 'null-result') {
        await expect(payload.locator('code')).toHaveText('null');
        await expect(payload).toContainText(side === 'Output' ? 'no output returned' : 'no input');
        await expect(payload).not.toContainText('Unavailable');
      } else {
        await expect(payload).toContainText('Oversize — omitted in full at the 64 KiB capture cap');
        await expect(payload).toContainText(side === 'Input' ? '70,000 bytes' : '80,000 bytes');
        await expect(payload).not.toContainText('Unavailable');
        await expect(payload.locator('pre')).toHaveCount(0);
      }
    }
    await expect(row.locator('.hk-description')).toHaveText('No description recorded for this run.');
    await expect(row).toContainText('Definition unavailable — no file path was recorded');
    await expect(row.getByRole('button', { name: 'Open definition', exact: true })).toHaveCount(0);
  }
});

test('mobile expanded inspection keeps long payload and definition inside viewport', async ({
  launchBough, controlRoom, page,
}) => {
  const b = await launchBough();
  const url = await controlRoom(b);
  const long = 'unbroken'.repeat(150);
  const definition = '/project/.bough/hooks/pre-tool-use/' + 'long-name-'.repeat(30) + '.js';
  await page.setViewportSize({ width: 375, height: 812 });
  await mockFires(page, [fire('mobile', { description: long, path: definition, input: { text: long }, output: { text: long } })]);
  await page.route('**/api/hooks/file?*', (route) => route.fulfill({ json: { path: definition, body: 'return null;' } }));
  await page.goto(url + '/#/hooks');
  const row = page.locator('.hk-fold');
  await row.locator(':scope > summary').click();
  await expectPayload(row, 'Input', { text: long });
  await expectPayload(row, 'Output', { text: long });
  await row.getByRole('button', { name: 'Open definition', exact: true }).click();
  await expect(row.getByRole('textbox', { name: 'File contents' })).toHaveValue('return null;');
  for (const selector of ['.proj-body', '.hk-inspect', '.hk-definition', '.hk-panel']) {
    const size = await page.locator(selector).evaluate((el) => ({
      scroll: el.scrollWidth, client: el.clientWidth, right: el.getBoundingClientRect().right,
    }));
    expect(size.scroll, selector + ' horizontal overflow').toBeLessThanOrEqual(size.client + 1);
    expect(size.right, selector + ' outside viewport').toBeLessThanOrEqual(376);
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(375);
});
