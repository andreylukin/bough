import { test, expect } from '../helpers/serve';

const id = '2026-09-19T08-00-00-00002';
test.use({ serveOpts: { home: {
  [`.bough/history/${id}.jsonl`]: [
    { seq: 1, kind: 'meta', data: { cwd: '/w/work', origin: 'web' } },
    { seq: 2, kind: 'input', data: { text: 'palette focus target' } },
    { seq: 3, kind: 'done', data: {} },
  ].map((entry) => JSON.stringify(entry)).join('\n') + '\n',
} } });

test('palette actions focus their destination and mark only the visible session current', async ({ page, serve }) => {
  await page.goto(serve.url);
  await expect(page.locator('#composer')).toBeVisible();

  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.getByRole('dialog', { name: 'Quick access' })).toBeVisible();
  await page.getByRole('option', { name: /New session in work/ }).click();
  const composer = page.locator('#composer');
  await expect(composer).toBeFocused();
  await page.keyboard.type('hello');
  await expect(composer).toHaveValue('hello');

  await page.goto(`${serve.url}/#/wiki`);
  await expect.poll(() => page.evaluate(() => location.hash)).toBe('#/wiki');
  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.getByRole('dialog', { name: 'Quick access' })).toBeVisible();
  await page.getByRole('combobox', { name: 'Search sessions or run a command' }).fill('after:1d');
  const target = page.locator(`[id="pal-s:${id}"]`);
  await expect(target).not.toContainText('Current');
  await target.click();
  await expect(composer).toBeFocused();

  await page.goto(`${serve.url}/#/wiki`);
  await expect.poll(() => page.evaluate(() => location.hash)).toBe('#/wiki');
  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.getByRole('dialog', { name: 'Quick access' })).toBeVisible();

  await page.getByRole('combobox', { name: 'Search sessions or run a command' }).fill('Hooks');
  await expect(page.getByRole('option', { name: /^Hooks/ })).toBeVisible();
  await page.getByRole('option', { name: /^Hooks/ }).click();
  await expect(page.getByRole('heading', { name: 'Hooks', level: 1 })).toBeFocused();
});
