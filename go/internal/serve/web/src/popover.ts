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
