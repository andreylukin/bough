import * as fs from 'fs';
import * as path from 'path';
import { test, expect } from '../helpers/serve';

for (const width of [1100, 390]) {
  test(`bundled context toolkit is discoverable and switchable at ${width}px`, async ({ serve, page }, info) => {
    await page.setViewportSize({ width, height: 844 });
    const id = await serve.newSession();
    const catalogue = await (await serve.api.get(`/api/skills?session=${id}`)).json();
    expect(catalogue.skills).toContainEqual(expect.objectContaining({ name: 'context-toolkit', source: 'builtin', manual: true }));
    for (const pool of ['.bough', '.claude']) {
      expect(fs.existsSync(path.join(serve.home, pool, 'skills', 'context-toolkit'))).toBe(false);
    }
    await page.goto(serve.url + '/#/s/' + id);
    await expect(page.locator('#composer')).toBeVisible();
    await page.getByRole('button', { name: 'Skills', exact: true }).click();
    await page.getByRole('option', { name: /^\/context-toolkit/ }).click();
    await expect(page.locator('#composer')).toHaveValue('/context-toolkit ');

    await page.locator('#composer').click();
    await page.keyboard.press('ControlOrMeta+k');
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.type('Inspect context');
    await page.getByRole('option', { name: /^Inspect context/ }).click();
    const row = page.locator('.hk2-row').filter({ has: page.getByText('/context-toolkit', { exact: true }) });
    await expect(row.locator('.hk2-tag')).toHaveText('Built-in');
    await info.attach(`context-toolkit-${width}`, { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
    await row.getByRole('button', { name: /Disable/ }).click();
    await expect(row).toHaveClass(/hk2-off/);
    const disabled = await (await serve.api.get(`/api/skills?session=${id}`)).json();
    expect(disabled.skills.some((s: { name: string }) => s.name === 'context-toolkit')).toBe(false);
    await row.getByRole('button', { name: /Enable/ }).click();
    await expect(row).not.toHaveClass(/hk2-off/);
    const enabled = await (await serve.api.get(`/api/skills?session=${id}`)).json();
    expect(enabled.skills.some((s: { name: string }) => s.name === 'context-toolkit')).toBe(true);
  });
}

for (const [name, overlay] of [
  ['append-only engine', '- id: loop\n  plugin: engine-unreal\n'],
  ['disabled toolkit row', '- id: context-tools\n  disabled: true\n'],
] as const) {
  test.describe(name, () => {
    test.use({ serveOpts: { config: '- id: llm\n  plugin: llm-echo\n' + overlay } });
    test('does not advertise the built-in toolkit skill', async ({ serve }) => {
      const catalogue = await (await serve.api.get('/api/skills')).json();
      expect(catalogue.skills.some((s: { name: string }) => s.name === 'context-toolkit')).toBe(false);
    });
  });
}
