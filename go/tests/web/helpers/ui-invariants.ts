// What every screen owes beyond its state, checked at a node of a model
// walk (Flow.check in helpers/model.ts): axe finds nothing on the
// surface, no text in a header or a status line is cut off, and the
// element with keyboard focus shows a ring.
import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';
import { expect } from './serve';

/**
 * axe on the elements `include` selects; every violation fails the node.
 * axe ends each rule on a setTimeout(0): the page's clock must be
 * running (installed is fine, paused is not).
 */
export async function axeClean(page: Page, include: string, where: string): Promise<void> {
  // Judged at rest: an entrance fade runs on real time (the page's fake
  // clock does not drive CSS animations), and axe measuring contrast
  // halfway through one reports a colour the screen never settles on.
  // A spinner repeats forever and is left running.
  await page.evaluate(() => Promise.all(document.getAnimations()
    .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
    .map((a) => a.finished.catch(() => {}))));
  const r = await new AxeBuilder({ page }).include(include).analyze();
  const found = r.violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => `${n.target.join(' ')}: ${n.failureSummary?.split('\n').slice(1).join(' ').trim()}`).join('; ')})`);
  expect(found, `${where}: axe on ${include}`).toEqual([]);
}

/**
 * Text cut off in the elements `selector` matches, and in every element
 * inside them: a box narrower or shorter than its content that hides the
 * rest (an ellipsis is a cut too). Inline boxes have no size of their
 * own and are skipped; their block parent carries the cut.
 */
export async function noClippedText(page: Page, selector: string, where: string): Promise<void> {
  const cut = await page.evaluate((sel) => {
    const out: string[] = [];
    for (const root of document.querySelectorAll<HTMLElement>(sel)) {
      for (const el of [root, ...root.querySelectorAll<HTMLElement>('*')]) {
        const cs = getComputedStyle(el);
        if (cs.display === 'inline' || cs.display === 'none' || cs.visibility === 'hidden') continue;
        if (!el.textContent?.trim() || el.getClientRects().length === 0) continue;
        const hides = (o: string) => o !== 'visible';
        const w = hides(cs.overflowX) && el.scrollWidth > el.clientWidth + 1;
        const h = hides(cs.overflowY) && el.scrollHeight > el.clientHeight + 1;
        if (w || h) out.push(`<${el.tagName.toLowerCase()} class="${el.className}"> "${el.textContent.trim().slice(0, 60)}"`);
      }
    }
    return out;
  }, selector);
  expect(cut, `${where}: clipped text in ${selector}`).toEqual([]);
}

/**
 * The element with keyboard focus (:focus-visible) draws a ring: an
 * outline or a box-shadow. A mouse click focuses a button without
 * :focus-visible, and then no ring is owed.
 */
export async function focusRingVisible(page: Page, where: string): Promise<void> {
  const bad = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body || !el.matches(':focus-visible')) return '';
    const cs = getComputedStyle(el);
    const outline = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0;
    const shadow = cs.boxShadow !== 'none' && cs.boxShadow !== '';
    return outline || shadow ? '' : `<${el.tagName.toLowerCase()} class="${el.className}"> has focus and no ring`;
  });
  expect(bad, `${where}: focus ring`).toBe('');
}
