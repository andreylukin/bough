import { test, expect } from '../helpers/serve';

test('palette actions focus their destination and mark only the visible session current', async ({ page, serve }) => {
  const id = await serve.newSession('palette focus target');
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
  await expect(page.getByRole('heading', { name: 'Wiki', level: 1 })).toBeVisible();
  await page.waitForTimeout(250);
  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.getByRole('dialog', { name: 'Quick access' })).toBeVisible();
  await page.getByRole('combobox', { name: 'Search sessions or run a command' }).fill('after:1d');
  const target = page.locator(`[id="pal-s:${id}"]`);
  await expect(target).not.toContainText('Current');
  await target.click();
  await expect(composer).toBeFocused();

  await page.goto(`${serve.url}/#/wiki`);
  await expect(page.getByRole('heading', { name: 'Wiki', level: 1 })).toBeVisible();
  await page.waitForTimeout(250);
  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.getByRole('dialog', { name: 'Quick access' })).toBeVisible();

  await page.getByRole('combobox', { name: 'Search sessions or run a command' }).fill('Hooks');
  await expect(page.getByRole('option', { name: /^Hooks/ })).toBeVisible();
  await page.getByRole('option', { name: /^Hooks/ }).click();
  await expect(page.getByRole('heading', { name: 'Hooks', level: 1 })).toBeFocused();
});
