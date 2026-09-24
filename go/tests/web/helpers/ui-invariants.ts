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
  // axe is most of a node's time, and a walk revisits the same screens:
  // markup already found clean at this size is not scanned again.
  const key = await page.evaluate((sels) => {
    let h = 0;
    const s = `${innerWidth}x${innerHeight}|` + sels.map((x) => document.querySelector(x)?.outerHTML ?? '').join('|');
    for (let i = 0; i < s.length; i++) h = (Math.imul(h, 31) + s.charCodeAt(i)) | 0;
    return `${s.length}:${h}`;
  }, on);
  if (clean.has(key)) return;
  // A note fading in reads as low contrast halfway: let finite
  // animations end first (a spinner never does, so it is not waited on).
  // The cap is on this side: the page's timers run on the walk's clock.
  await Promise.race([
    page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
      .map((a) => a.finished.then(() => undefined, () => undefined)))),
    new Promise((r) => setTimeout(r, 1000)),
  ]);
  // Only violations are read: axe skips detailing what passed.
  let axe = new AxeBuilder({ page }).options({ resultTypes: ['violations'] });
  for (const s of on) axe = axe.include(s);
  const res = await axe.analyze();
  const found = res.violations.map((v) =>
    `${v.id} (${v.impact}): ${v.help} — ${v.nodes.map((n) => n.target.join(' ')).slice(0, 4).join(', ')}`);
  expect(found, `${where}: axe`).toEqual([]);
  clean.add(key);
}

const clean = new Set<string>();
