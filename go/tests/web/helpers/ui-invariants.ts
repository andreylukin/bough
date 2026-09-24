// What a screen owes at every node of a model walk beyond the shared
// checks in helpers/model.ts (no sideways scroll, status visible, no
// console errors): its header and status text are not cut off, the
// element keyboard focus is on shows a ring, and axe finds nothing on
// the surface. A data-only walk let clipped headers and invisible focus
// ship; these are the checks that see them.
import AxeBuilder from '@axe-core/playwright';
import type { Page } from '@playwright/test';
import { expect } from './serve';

export interface Surface {
  /** CSS selectors axe scans (those not on screen are skipped). */
  include: string[];
  /** Elements whose text must not be clipped: headers, status lines. */
  text: string;
}

/** Visible elements under `sel` whose own box cuts their text. */
async function clipped(page: Page, sel: string): Promise<string[]> {
  return page.evaluate((sel) => {
    const out: string[] = [];
    for (const el of Array.from(document.querySelectorAll<HTMLElement>(sel))) {
      if (!el.getClientRects().length || !el.textContent?.trim()) continue;
      const cs = getComputedStyle(el);
      if (cs.visibility === 'hidden') continue;
      // Only a box that hides what spills out clips it; a visible
      // overflow is the sideways-scroll check's business.
      const hidesX = cs.overflowX !== 'visible' || cs.textOverflow === 'ellipsis';
      const hidesY = cs.overflowY !== 'visible';
      const wide = el.scrollWidth - el.clientWidth > 1;
      const tall = el.scrollHeight - el.clientHeight > 1;
      if ((hidesX && wide) || (hidesY && tall)) {
        out.push(`<${el.tagName.toLowerCase()} class="${el.className}"> "${el.textContent.trim().slice(0, 40)}" ` +
          `${el.scrollWidth}x${el.scrollHeight} in ${el.clientWidth}x${el.clientHeight}`);
      }
    }
    return out;
  }, sel);
}

/** The focused element, when the browser would draw a focus ring for it, and whether one shows. */
async function focusRing(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body || !el.matches(':focus-visible')) return '';
    if (!el.getClientRects().length) return '';
    // A container out of the tab order that the page parks focus on (a
    // route change puts it on .app, so the next Tab reaches the skip
    // links) is not a control: a ring round the whole page is not owed.
    if (el.tabIndex < 0 && !el.matches('input, textarea, select, [contenteditable]')) return '';
    const cs = getComputedStyle(el);
    // A text field shows focus with its caret: the palette's field drops
    // the ring on purpose (4bd7ba86, "one accent per state").
    const typing = el.isContentEditable || el.tagName === 'TEXTAREA' ||
      (el.tagName === 'INPUT' && /^(text|search|email|url|tel|password|)$/.test((el as HTMLInputElement).type));
    if (typing && cs.caretColor !== 'transparent') return '';
    const outline = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0;
    const shadow = cs.boxShadow !== 'none' && cs.boxShadow !== '';
    if (outline || shadow) return '';
    return `<${el.tagName.toLowerCase()} class="${el.className}" aria-label="${el.getAttribute('aria-label') ?? ''}"> has focus and no visible ring`;
  });
}

export async function uiInvariants(page: Page, surface: Surface, where: string): Promise<void> {
  expect(await clipped(page, surface.text), `${where}: clipped text`).toEqual([]);
  expect(await focusRing(page), `${where}: focus ring`).toBe('');
  const on: string[] = [];
  for (const s of surface.include) if (await page.locator(s).count()) on.push(s);
  if (on.length === 0) return;
  let axe = new AxeBuilder({ page });
  for (const s of on) axe = axe.include(s);
  const res = await axe.analyze();
  const found = res.violations.map((v) =>
    `${v.id} (${v.impact}): ${v.help} — ${v.nodes.map((n) => n.target.join(' ')).slice(0, 4).join(', ')}`);
  expect(found, `${where}: axe`).toEqual([]);
}

// The same checks one at a time, for a flow that calls them from its own
// Flow.check (specs/model/ui_me.spec.ts).

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
