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

const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGNgAAAAAgABSK+kcQAAAABJRU5ErkJggg==';

async function pasteScreenshot(target: import('@playwright/test').Locator, options: { type?: string; size?: number } = {}) {
  return target.evaluate((el, { png, type, size }) => {
    const bytes = size === undefined ? Uint8Array.from(atob(png), (c) => c.charCodeAt(0)) : new Uint8Array(size);
    const data = new DataTransfer();
    data.items.add(new File([bytes], 'clipboard.png', { type }));
    data.setData('text/plain', 'image fallback text');
    const event = new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true });
    el.dispatchEvent(event);
    return event.defaultPrevented;
  }, { png: PNG, type: options.type ?? 'image/png', size: options.size });
}

const preview = (dialog: import('@playwright/test').Locator) => dialog.getByAltText('Screenshot selected for review');
const copy = (dialog: import('@playwright/test').Locator) => dialog.getByRole('button', { name: 'Copy screenshot', exact: true });

async function watchObjectURLs(page: import('@playwright/test').Page) {
  await page.evaluate(() => {
    const revoke = URL.revokeObjectURL.bind(URL);
    (window as any).revokedScreenshots = [];
    URL.revokeObjectURL = (url) => { (window as any).revokedScreenshots.push(url); revoke(url); };
  });
}

async function expectRevoked(page: import('@playwright/test').Page, url: string | null) {
  await expect.poll(() => page.evaluate((url) => (window as any).revokedScreenshots.includes(url), url)).toBe(true);
}

test('clipboard paste replaces a reviewed screenshot and removal releases it without uploading', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  const uploads: string[] = [];
  page.on('request', (request) => { if (request.method() === 'POST') uploads.push(request.url()); });
  await watchObjectURLs(page);
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  await dialog.getByLabel('Issue title').fill('Paste screenshot');
  await dialog.getByLabel('What happened').fill('Keep this text');
  expect(await pasteScreenshot(dialog.getByLabel('What happened'))).toBe(true);
  await expect(copy(dialog)).toBeEnabled();
  await expect(dialog.getByLabel('What happened')).toHaveValue('Keep this text');
  const first = await preview(dialog).getAttribute('src');
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await copy(dialog).click();
  await expect(dialog).toContainText('Copied. Paste it into the GitHub issue');

  expect(await pasteScreenshot(dialog.getByLabel('Issue title'))).toBe(true);
  await expect(copy(dialog)).toBeEnabled();
  await expect(preview(dialog)).not.toHaveAttribute('src', first!);
  await expect(preview(dialog)).toHaveCount(1);
  await expect(dialog.getByLabel('Issue title')).toHaveValue('Paste screenshot');
  await expect(dialog).not.toContainText('Copied.');
  await expectRevoked(page, first);
  const second = await preview(dialog).getAttribute('src');
  await dialog.getByRole('button', { name: 'Remove screenshot' }).click();
  await expect(preview(dialog)).toHaveCount(0);
  await expect(copy(dialog)).toHaveCount(0);
  await expectRevoked(page, second);
  await expect(dialog.getByLabel('Screenshot (optional, PNG)')).toBeFocused();

  const file = { name: 'same.png', mimeType: 'image/png', buffer: Buffer.from(PNG, 'base64') };
  await dialog.getByLabel('Screenshot (optional, PNG)').setInputFiles(file);
  await expect(copy(dialog)).toBeEnabled();
  await dialog.getByRole('button', { name: 'Remove screenshot' }).click();
  await dialog.getByLabel('Screenshot (optional, PNG)').setInputFiles(file);
  await expect(copy(dialog)).toBeEnabled();
  const chosen = await preview(dialog).getAttribute('src');
  await dialog.getByLabel('Screenshot (optional, PNG)').setInputFiles([]);
  await expect(preview(dialog)).toHaveAttribute('src', chosen!);
  expect(uploads).toEqual([]);
});

test('native keyboard paste accepts a screenshot and ordinary text still pastes normally', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  await page.evaluate(async (png) => {
    const bytes = Uint8Array.from(atob(png), (c) => c.charCodeAt(0));
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': new Blob([bytes], { type: 'image/png' }) })]);
  }, PNG);
  await dialog.getByLabel('What happened').focus();
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+V' : 'Control+V');
  await expect(copy(dialog)).toBeEnabled();
  await page.evaluate(() => navigator.clipboard.writeText('ordinary pasted text'));
  await page.keyboard.press(process.platform === 'darwin' ? 'Meta+V' : 'Control+V');
  await expect(dialog.getByLabel('What happened')).toHaveValue('ordinary pasted text');
  await expect(preview(dialog)).toHaveCount(1);
});

test('clipboard failure keeps a downloadable screenshot through the reviewed GitHub handoff', async ({ sharedServe, page, context }) => {
  await page.goto(sharedServe.url);
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  await dialog.getByLabel('Issue title').fill('Clipboard blocked');
  await dialog.getByLabel('What happened').fill('Attach the reviewed image');
  await pasteScreenshot(dialog.getByLabel('What happened'));
  await expect(copy(dialog)).toBeEnabled();
  await page.evaluate(() => {
    Object.defineProperty(navigator.clipboard, 'write', { configurable: true, value: () => Promise.reject(new Error('permission denied')) });
  });
  await copy(dialog).click();
  await expect(dialog.getByRole('alert')).toContainText('Download it, then attach the file in GitHub.');
  const downloading = page.waitForEvent('download');
  await dialog.getByRole('link', { name: 'Download screenshot' }).click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe('bough-feedback.png');
  const { readFileSync } = await import('fs');
  expect(readFileSync((await download.path())!).toString('base64')).toBe(PNG);

  // No public issue or image is posted by this test.
  await context.route('https://github.com/andreylukin/bough/issues/new?**', (route) => route.fulfill({ contentType: 'text/html', body: 'GitHub draft' }));
  const opening = page.waitForEvent('popup');
  await dialog.getByRole('link', { name: 'Open GitHub issue' }).click();
  const github = await opening;
  await github.waitForLoadState();
  expect(new URL(github.url()).searchParams.get('body')).toBe('## What happened\nAttach the reviewed image\n\n## Environment\nbough web');
  await github.close();
  await expect(preview(dialog)).toBeVisible();
  await expect(dialog).toContainText('Opening the issue does not attach the image.');
  await expect(dialog.getByRole('link', { name: 'Download screenshot' })).toBeVisible();
});

test('invalid screenshots are explained and cannot be copied as PNG', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  const target = dialog.getByLabel('What happened');
  await pasteScreenshot(target);
  await expect(copy(dialog)).toBeEnabled();
  const valid = await preview(dialog).getAttribute('src');
  await pasteScreenshot(target, { type: 'image/jpeg' });
  await expect(dialog.getByRole('alert')).toHaveText('Use a PNG screenshot.');
  await expect(preview(dialog)).toHaveAttribute('src', valid!);
  await pasteScreenshot(target, { size: 0 });
  await expect(dialog.getByRole('alert')).toContainText('empty');
  await pasteScreenshot(target, { size: 10 * 1024 * 1024 + 1 });
  await expect(dialog.getByRole('alert')).toContainText('10 MB');
  await dialog.getByLabel('Screenshot (optional, PNG)').setInputFiles({ name: 'fake.png', mimeType: 'image/png', buffer: Buffer.from('not an image') });
  await expect(dialog.getByRole('alert')).toContainText('Could not read the screenshot.');
  await expect(preview(dialog)).toHaveCount(0);
  await expect(copy(dialog)).toHaveCount(0);
  await pasteScreenshot(target);
  await expect(copy(dialog)).toBeEnabled();
  await expect(dialog.getByRole('alert')).toHaveCount(0);
});

test('late clipboard writes cannot mark a replaced, removed or reopened screenshot as copied', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  await watchObjectURLs(page);
  const opener = page.locator('.side-feedback');
  await opener.click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  const holdCopy = () => page.evaluate(() => {
    Object.defineProperty(navigator.clipboard, 'write', { configurable: true, value: () => new Promise<void>((resolve, reject) => {
      (window as any).finishCopy = resolve; (window as any).failCopy = () => reject(new Error('late failure'));
    }) });
  });
  await holdCopy();
  await pasteScreenshot(dialog.getByLabel('What happened'));
  await expect(copy(dialog)).toBeEnabled();
  await copy(dialog).click();
  await expect(dialog.getByRole('button', { name: 'Copying…' })).toBeDisabled();
  await pasteScreenshot(dialog.getByLabel('What happened'));
  await expect(copy(dialog)).toBeEnabled();
  await page.evaluate(() => (window as any).finishCopy());
  await expect(dialog).not.toContainText('Copied.');

  await copy(dialog).click();
  await dialog.getByRole('button', { name: 'Remove screenshot' }).click();
  await page.evaluate(() => (window as any).failCopy());
  await expect(dialog.getByRole('alert')).toHaveCount(0);
  await expect(preview(dialog)).toHaveCount(0);

  await pasteScreenshot(dialog.getByLabel('What happened'));
  await expect(copy(dialog)).toBeEnabled();
  const old = await preview(dialog).getAttribute('src');
  await copy(dialog).click();
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
  await expectRevoked(page, old);
  await expect(opener).toBeFocused();
  await opener.click();
  await pasteScreenshot(dialog.getByLabel('What happened'));
  await expect(copy(dialog)).toBeEnabled();
  await page.evaluate(() => (window as any).finishCopy());
  await expect(dialog).not.toContainText('Copied.');
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await opener.click();
  await expect(preview(dialog)).toHaveCount(0);
  await expect(dialog.getByLabel('Issue title')).toHaveValue('');
  await expect(dialog.getByLabel('What happened')).toHaveValue('');
});

test('explicit screenshot submission sends the reviewed PNG and opens the created issue', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  let submissions = 0;
  await page.route('**/api/feedback', async (route) => {
    submissions++;
    const request = route.request();
    expect(request.method()).toBe('POST');
    expect(request.headers()['content-type']).toContain('multipart/form-data; boundary=');
    const payload = request.postDataBuffer()!;
    expect(payload.includes(Buffer.from(PNG, 'base64'))).toBe(true);
    expect(payload.toString()).toContain('Public synthetic report');
    expect(payload.toString()).toContain('Reviewed details');
    expect(payload.toString()).toContain('name="share"\r\n\r\npublic');
    expect(payload.toString()).not.toContain('blob:');
    await route.fulfill({ status: 201, json: { url: 'https://github.com/andreylukin/bough/issues/123', retryable: false } });
  });
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  await dialog.getByLabel('Issue title').fill('Public synthetic report');
  await dialog.getByLabel('What happened').fill('Reviewed details');
  await pasteScreenshot(dialog);
  await expect(copy(dialog)).toBeEnabled();
  const submit = dialog.getByRole('button', { name: 'Submit with screenshot' });
  await expect(submit).toBeDisabled();
  expect(submissions).toBe(0);
  await dialog.getByRole('checkbox').check();
  await dialog.getByLabel('What happened').fill('Reviewed details updated');
  await expect(dialog.getByRole('checkbox')).not.toBeChecked();
  await expect(submit).toBeDisabled();
  await dialog.getByRole('checkbox').check();
  await submit.click();
  await expect(dialog.getByRole('status')).toContainText('Issue created with screenshot');
  await expect(dialog.getByRole('link', { name: 'View GitHub issue' })).toHaveAttribute('href', 'https://github.com/andreylukin/bough/issues/123');
  await expect(submit).toHaveCount(0);
  await expect(dialog.getByRole('link', { name: 'Open GitHub issue' })).toHaveCount(0);
  expect(submissions).toBe(1);
  await expect(preview(dialog)).toBeVisible();
});

test('unavailable gh preserves the screenshot; an uncertain submission cannot be retried', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  let submissions = 0;
  await page.route('**/api/feedback', async (route) => {
    submissions++;
    if (submissions === 1) await route.fulfill({ status: 503, json: { error: 'Update GitHub CLI to support --attach.', retryable: true } });
    else await route.abort();
  });
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  await dialog.getByLabel('Issue title').fill('Synthetic submission failure');
  await dialog.getByLabel('What happened').fill('Keep the reviewed image');
  await pasteScreenshot(dialog);
  await expect(copy(dialog)).toBeEnabled();
  await dialog.getByRole('checkbox').check();
  const submit = dialog.getByRole('button', { name: 'Submit with screenshot' });
  await submit.click();
  await expect(dialog.getByRole('alert')).toContainText('Update GitHub CLI');
  await expect(submit).toBeEnabled();
  await expect(preview(dialog)).toBeVisible();
  await expect(dialog.getByRole('link', { name: 'Download screenshot' })).toBeVisible();
  await submit.click();
  await expect(dialog.getByRole('alert')).toContainText('Check andreylukin/bough issues before submitting again');
  await expect(submit).toHaveCount(0);
  await expect(dialog.getByRole('link', { name: 'Open GitHub issue' })).toHaveCount(0);
  await expect(preview(dialog)).toBeVisible();
  expect(submissions).toBe(2);
});

test('pending submission locks the reviewed report and prevents duplicate submits', async ({ sharedServe, page }) => {
  await page.goto(sharedServe.url);
  let finish!: () => void;
  const pending = new Promise<void>((resolve) => { finish = resolve; });
  let submissions = 0;
  await page.route('**/api/feedback', async (route) => {
    submissions++;
    await pending;
    await route.fulfill({ status: 201, json: { url: 'https://github.com/andreylukin/bough/issues/123' } });
  });
  await page.locator('.side-feedback').click();
  const dialog = page.getByRole('dialog', { name: 'Send feedback' });
  await dialog.getByLabel('Issue title').fill('One synthetic submission');
  await dialog.getByLabel('What happened').fill('Keep this exact report');
  await pasteScreenshot(dialog);
  await expect(copy(dialog)).toBeEnabled();
  await dialog.getByRole('checkbox').check();
  await dialog.getByRole('button', { name: 'Submit with screenshot' }).click();
  await expect(dialog.getByRole('button', { name: 'Submitting…' })).toBeDisabled();
  await expect(dialog.getByLabel('Issue title')).toBeDisabled();
  await expect(dialog.getByRole('button', { name: 'Remove screenshot' })).toBeDisabled();
  await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeVisible();
  expect(submissions).toBe(1);
  finish();
  await expect(dialog.getByRole('status')).toContainText('Issue created with screenshot');
});
