import { test, expect } from '../helpers/serve';

// Synthetic #89 reproduction; no private session data.
for (const width of [1100, 390]) {
  test(`tool totals and failure units remain readable at ${width}px`, async ({ sharedServe, page }, info) => {
    await page.setViewportSize({ width, height: 844 });
    const at = '2026-10-02T12:00:00Z';
    const entries = [{ seq: 1, at, kind: 'input', text: 'Inspect the example.' },
      ...Array.from({ length: 144 }, (_, i) => ({ seq: i + 2, at, kind: 'call', text: i < 2 ? 'Example.ts' : i < 20 ? 'grep example' : 'Inspect context',
        data: { id: `call-${i}`, tool: i < 2 ? 'view' : i < 20 ? 'bash' : 'context_inspect', ms: 1000, ...(i >= 2 && i < 20 ? { exit: 1, error: 'exit status 1' } : {}), output: 'Recorded output.' } })),
      { seq: 146, at, kind: 'assistant', text: 'The inspection is complete.' }, { seq: 147, at, kind: 'done', text: '' }];
    const row = { id: 'tool-counts', title: 'Inspect the example', cwd: '/w/example', status: 'done', live: false, archived: false, entries: entries.length, modified: at, lastAt: at, mode: 'local' };
    await page.route(/\/api\/sessions(\?.*)?$/, r => r.fulfill({ json: { sessions: [row] } }));
    await page.route(/\/api\/sessions\/tool-counts(\?.*)?$/, r => r.fulfill({ json: { session: row, entries } }));
    await page.route(/\/api\/sessions\/tool-counts\/events$/, r => r.abort());
    await page.route(/\/api\/sessions\/tool-counts\/children$/, r => r.fulfill({ json: { children: [] } }));
    await page.goto(sharedServe.url + '/#/s/tool-counts');
    const summary = page.locator('.toolrun > summary');
    await expect(summary).toBeVisible();
    await expect(summary).toContainText('Includes: read 2 files · ran 18 commands');
    await expect(summary).toContainText('144 tool calls');
    const failed = page.getByRole('button', { name: '18 calls failed', exact: true });
    await expect(failed).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const total = await summary.locator('.tool-meta').boundingBox();
    const failure = await failed.boundingBox();
    expect(total!.x + total!.width).toBeLessThanOrEqual(failure!.x);
    await info.attach(`tool-counts-${width}`, { body: await page.screenshot({ path: info.outputPath(`tool-counts-${width}.png`) }), contentType: 'image/png' });
    await failed.focus();
    await failed.press('Enter');
    const firstFailed = page.locator('.call-native.block-failed').first();
    await expect(firstFailed).toHaveAttribute('open');
    await expect(firstFailed.locator(':scope > summary')).toBeFocused();
    await expect(firstFailed).toContainText('exit 1');
  });
}
