import { test, expect } from '../helpers/serve';

test('web feedback previews a chosen screenshot and prepares a reviewed GitHub issue', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('a', { hasText: 'Open GitHub issue' })).not.toHaveAttribute('href', /github/);
  await dialog.getByLabel('Issue title').fill('Web page is blank');
  await dialog.getByLabel('What happened').fill('After reload, the page is blank.');
  const png = await page.screenshot();
  await dialog.getByLabel('Screenshot (optional, PNG)').setInputFiles({ name: 'screen.png', mimeType: 'image/png', buffer: png });
  await expect(dialog.getByAltText('Screenshot selected for review')).toBeVisible();
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await dialog.getByRole('button', { name: 'Copy screenshot' }).click();
  await expect(dialog).toContainText('Copied. Paste it into the GitHub issue');
  const href = await dialog.getByRole('link', { name: 'Open GitHub issue' }).getAttribute('href');
  const url = new URL(href!);
  expect(url.origin + url.pathname).toBe('https://github.com/andreylukin/bough/issues/new');
  expect(url.searchParams.get('title')).toBe('Web page is blank');
  expect(url.searchParams.get('body')).toContain('After reload, the page is blank.');
  expect(url.searchParams.get('body')).not.toContain('screen.png');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 780 });
  await page.getByRole('button', { name: 'Skip the welcome' }).click();
  await page.locator('.phone-nav').getByRole('button', { name: 'Send feedback' }).click();
  await expect(page.getByRole('dialog', { name: 'Send feedback' })).toBeVisible();
});
