import { test, expect } from '../helpers/serve';

const id = '2026-09-19T08-00-00-00001';

test.use({ serveOpts: { home: {
  '.bough/projects/orbit/project.yml': 'name: Orbit\nrepos: []\n',
  [`.bough/history/${id}.jsonl`]: [
    { seq: 1, kind: 'meta', data: { cwd: '/w/repo' } },
    { seq: 2, kind: 'input', data: { text: 'Fix the calendar date normalization' } },
    { seq: 3, kind: 'done', data: {} },
  ].map((entry) => JSON.stringify(entry)).join('\n') + '\n',
  '.bough/serve/meta.json': JSON.stringify({ version: 2, sessions: { [id]: {} } }),
} } });

test('project session controls remain readable at phone width', async ({ serve, page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(serve.url + '/#/projects');
  const row = page.locator('.proj-row').filter({ hasText: 'Fix the calendar date normalization' });
  await expect(row).toBeVisible();
  const move = row.getByRole('combobox', { name: /Move to project/ });
  await expect(move).toContainText('Move…');
  await expect(row.locator('.proj-move')).toHaveCSS('opacity', '1');
  await expect(row.locator('.proj-check input')).toHaveCSS('opacity', '1');
  await move.click();
  await expect(page.getByRole('option', { name: 'Orbit' })).toBeVisible();
});
