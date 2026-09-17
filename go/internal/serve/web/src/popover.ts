/**
 * One placement rule for anchored popovers (edits, Work): keep them inside
 * the viewport with an 8px margin. When too wide for it, the left edge wins.
 */
export function clampShift(left: number, right: number, viewport: number, margin = 8): number {
  if (right > viewport - margin) return Math.max(viewport - margin - right, margin - left);
  if (left < margin) return margin - left;
  return 0;
}

/** Shifts an already-placed popover sideways into the viewport. */
export function clampToViewport(el: HTMLElement | null) {
  if (!el) return;
  el.style.translate = "";
  // From the layout width: an entry animation's scale shrinks the rect.
  const r = el.getBoundingClientRect(), mid = (r.left + r.right) / 2, w = el.offsetWidth;
  const dx = clampShift(mid - w / 2, mid + w / 2, document.documentElement.clientWidth);
  if (dx) el.style.translate = `${dx}px 0`;
}

export interface Box { left: number; right: number; top: number; bottom: number }

/**
 * Where a popover of `w`×`h` goes against its trigger: `gap` below it, or
 * above when it does not fit below and there is more room there. Its
 * `align` edge lines up with the trigger's; if that leaves `bounds` (the
 * pane it belongs to, within the viewport), the other edge is tried, then
 * it is clamped with an 8px margin. maxHeight is the room on its side, so
 * a tall popover scrolls inside rather than being cut off.
 */
export function anchorPlace(t: Box, w: number, h: number, bounds: Box, align: "start" | "end" = "start", gap = 4, margin = 8) {
  const lo = bounds.left + margin, hi = bounds.right - margin;
  const fits = (l: number) => l >= lo && l + w <= hi;
  const first = align === "end" ? t.right - w : t.left, other = align === "end" ? t.left : t.right - w;
  // Too wide for the bounds: the start edge wins, as clampShift does.
  const left = fits(first) ? first : fits(other) ? other : Math.max(lo, Math.min(first, hi - w));
  const below = bounds.bottom - margin - t.bottom - gap, above = t.top - gap - bounds.top - margin;
  const up = h > below && above > below;
  const maxHeight = Math.max(0, up ? above : below);
  const top = up ? t.top - gap - Math.min(h, maxHeight) : t.bottom + gap;
  return { left, top, maxHeight, up };
}
