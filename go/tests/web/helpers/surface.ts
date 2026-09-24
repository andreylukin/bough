// What a surface owes at every node of a model walk, beyond the shared
// invariants in helpers/model.ts: text that is cut off says less than
// it looks like it does, a focused control nobody can see is a keyboard
// user lost, and axe finds the rest a screen reader trips over.
import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';
import { expect } from './serve';

export interface Surface {
  /** The surface axe scans (selectors; ones absent at a node are skipped). */
  include: string[];
  /** Text that must never be clipped: headers, titles, the status. */
  texts: string;
}

/** Every visible element of `texts` shows all of its text. */
export async function noClippedText(page: Page, texts: string, where: string): Promise<void> {
  const clipped = await page.evaluate((sel) => {
    const out: string[] = [];
    for (const el of document.querySelectorAll<HTMLElement>(sel)) {
      if (el.getClientRects().length === 0) continue;
      if (el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1) {
        out.push(`${el.tagName.toLowerCase()}.${el.className}: "${el.textContent?.trim().slice(0, 60)}" (${el.scrollWidth}x${el.scrollHeight} in ${el.clientWidth}x${el.clientHeight})`);
      }
    }
    return out;
  }, texts);
  expect(clipped, `${where}: clipped text`).toEqual([]);
}

/**
 * The focused element shows that it is focused whenever the browser
 * says a ring is due (:focus-visible: always for a text box, after a
 * keyboard move for a button). A mouse click on a button owes none.
 */
export async function focusRingVisible(page: Page, where: string): Promise<void> {
  const bad = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body || !el.matches(':focus-visible')) return '';
    const cs = getComputedStyle(el);
    const outline = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0;
    if (outline || cs.boxShadow !== 'none') return '';
    return el.outerHTML.slice(0, 120);
  });
  expect(bad, `${where}: focused element has no visible focus ring`).toBe('');
}

/** axe finds no violation in what the surface shows now. */
export async function axeClean(page: Page, include: string[], where: string): Promise<void> {
  const present: string[] = [];
  for (const sel of include) if (await page.locator(sel).count()) present.push(sel);
  if (present.length === 0) return;
  // A dialog fading in reads as low contrast half way: judge what it
  // settles on. CSS animations run on real time, not the walk's clock,
  // so the wait is bounded here rather than by a timer in the page.
  await Promise.race([
    page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
      .map((a) => a.finished.then(() => {}, () => {})))),
    new Promise((r) => setTimeout(r, 2_000)),
  ]);
  // Legacy mode runs axe.run in the page itself; the default opens a
  // blank page per call to merge frames, and this surface has none.
  let b = new AxeBuilder({ page }).setLegacyMode().options({ resultTypes: ['violations'] });
  for (const sel of present) b = b.include(sel);
  const { violations } = await b.analyze();
  const said = violations.map((v) => `${v.id}: ${v.help} — ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`);
  expect(said, `${where}: axe`).toEqual([]);
}

export async function surfaceChecks(page: Page, s: Surface, where: string): Promise<void> {
  await noClippedText(page, s.texts, where);
  await focusRingVisible(page, where);
  await axeClean(page, s.include, where);
}
