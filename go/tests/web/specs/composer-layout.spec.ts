import { readFileSync } from 'fs';
import * as path from 'path';
import { test, expect } from '@playwright/test';

const html = readFileSync(path.resolve(__dirname, '../../../internal/serve/web/dist/index.html'), 'utf8');
const style = html.match(/<style>([\s\S]*?)<\/style>/)![1];

for (const width of [1100, 760, 390]) {
  test(`running composer keeps its send label inside the button at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 700 });
    await page.setContent(`
      <style>${style}</style>
      <div class="composer-wrap">
        <div class="composer">
          <textarea id="composer" rows="1">i</textarea>
          <div class="composer-bar">
            <span class="hint">@ files · / skills · ↵ send</span>
            <div class="composer-actions">
              <button class="btn">Skills</button>
              <button class="btn">Stop</button>
              <button class="btn primary">Send to running</button>
            </div>
          </div>
        </div>
      </div>
    `);
    const send = page.getByRole('button', { name: 'Send to running' });
    const layout = await send.evaluate((button) => {
      const range = document.createRange();
      range.selectNodeContents(button);
      const text = range.getBoundingClientRect();
      const box = button.getBoundingClientRect();
      const composer = button.closest('.composer')!.getBoundingClientRect();
      return {
        lines: range.getClientRects().length,
        fits: text.left >= box.left && text.right <= box.right &&
          text.top >= box.top && text.bottom <= box.bottom,
        insideComposer: box.right <= composer.right,
      };
    });
    expect(layout).toEqual({ lines: 1, fits: true, insideComposer: true });
  });
}
