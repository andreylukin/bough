import { test, expect } from '../helpers/serve';

test('finished sessions show their last result in the list', async ({ sharedServe, page }) => {
  const now = new Date().toISOString();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.route(/\/api\/sessions(\?.*)?$/, (route) =>
    route.fulfill({ json: { sessions: [{ id: 'finished', title: 'Review tests', outcome: 'Found three failing tests in src/todos.test.js', cwd: '/w/bough', status: 'done', live: false, archived: false, entries: 5, modified: now, lastAt: now, mode: 'local' }] } }));
  await page.goto(sharedServe.url + '/');
  const row = page.getByRole('treeitem', { name: /Review tests/ });
  await expect(row.locator('.row-meta')).toBeVisible();
  await expect(row.locator('.row-meta')).toContainText('Found three failing tests in src/todos.test.js');
  await expect(row).toHaveAttribute('aria-label', /Found three failing tests in src\/todos.test.js/);
});
