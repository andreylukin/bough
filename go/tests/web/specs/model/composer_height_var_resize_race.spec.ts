// specs/composer_height_var_resize_race.fizz (recipe: go/tests/model/README.md):
// the --composer-h CSS variable a ResizeObserver on the composer writes,
// racing the composer's own layout effect when an attachment is added
// or removed. Attaching drops a File onto the composer the way a drag
// does, which grows the textarea (its own layout effect, synchronous)
// before the ResizeObserver on .composer-wrap has a chance to fire and
// catch up --composer-h; the walk's FireObserver step lets it.
import type { Page } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  // Height of .composer-wrap with nothing attached, measured once at
  // Init: the DOM's only baseline for turning a pixel height back into
  // the spec's tall/cssVar booleans.
  shortH: number;
}

// A 1x1 PNG: small enough that the attach it drives really uploads.
const PNG_B64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';

const wrap = (c: Ctx) => c.page.locator('.composer-wrap');

// The composer's live height (what its own layout effect just did) and
// the CSS variable the ResizeObserver last wrote (what the padding
// reads) — both off the DOM, never the React state or an API.
async function measure(c: Ctx): Promise<{ live: number; css: number }> {
  return c.page.evaluate(() => {
    const el = document.querySelector('.composer-wrap') as HTMLElement | null;
    const live = el ? el.offsetHeight + 16 : 0;
    const css = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--composer-h')) || 0;
    return { live, css };
  });
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { live, css } = await measure(c);
  const draftText = await c.page.locator('#composer').inputValue();
  const pendingResize = await c.page.evaluate(() => ((window as unknown as { __roQueue?: unknown[] }).__roQueue?.length ?? 0) > 0);
  // Grown by more than a few px past the empty baseline: the attach tag
  // wrapped the textarea onto a second line.
  const tall = live - c.shortH > 5;
  return {
    attached: draftText.includes('[Image #'),
    tall,
    cssVar: css - c.shortH > 5,
    pendingResize,
  };
}

// The real ResizeObserver notifies within a frame or two of the size
// change — far too fast for a round trip through the page to ever catch
// it "still queued". This installs it before the app's own script runs,
// so its callback is the genuine native one, on the genuine native
// entries, just held in a queue instead of run immediately: exactly the
// "coalesces into the callback already scheduled" the spec's header
// describes, made steppable. window.__fireResizeObservers() is
// FireObserver; __roQueue's length is pendingResize.
// Only the composer-wrap's own notifications are queued: the page has
// other ResizeObservers (the pane-narrow watcher, the head/main one) and
// they run as normal, unheld, so pendingResize means only what the spec
// names it for.
const QUEUE_RESIZE_OBSERVER = `
  window.__NativeResizeObserver = window.ResizeObserver;
  window.__roQueue = [];
  window.ResizeObserver = class {
    constructor(cb) {
      this._native = new window.__NativeResizeObserver((entries, obs) => {
        const relevant = entries.some((e) => e.target.classList && e.target.classList.contains('composer-wrap'));
        if (relevant) window.__roQueue.push(() => cb(entries, obs));
        else cb(entries, obs);
      });
    }
    observe(...a) { this._native.observe(...a); }
    unobserve(...a) { this._native.unobserve(...a); }
    disconnect(...a) { this._native.disconnect(...a); }
  };
  window.__fireResizeObservers = () => {
    const q = window.__roQueue;
    window.__roQueue = [];
    q.forEach((fn) => fn());
  };
`;

const scripted = new WeakSet<Page>();

// Drops real files on the composer the way a drag does: the same onDrop
// handler a person's drag fires, so the tags it inserts and the uploads
// it starts are both real. Several at once, so the tags they leave in
// the draft (each "[Image #N] ") wrap the textarea onto a second line —
// one file's tag alone is short enough to fit on one line, and then
// nothing about attaching grows the composer at all.
async function drop(c: Ctx): Promise<void> {
  await c.page.locator('.composer').evaluate((el, b64) => {
    const bytes = Uint8Array.from(atob(b64), (ch) => ch.charCodeAt(0));
    const dt = new DataTransfer();
    for (let i = 0; i < 8; i++) dt.items.add(new File([bytes], `a${i}.png`, { type: 'image/png' }));
    el.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt }));
  }, PNG_B64);
}

modelTests<Ctx>({
  spec: 'composer_height_var_resize_race',
  role: 'Composer#0',
  shared: true,
  reset: true,
  // The spec has no polled list state; a step's own read is what moved,
  // not the 4 s/12 s list poll.
  pollStepMs: 0,

  async init(page, serve) {
    // A wide desktop composer fits all eight tags on one line; nothing
    // about attaching would grow it then. Narrow enough that they wrap.
    await page.setViewportSize({ width: 375, height: 700 });
    // shared:true reuses this page across walks: add the init script once,
    // not once per walk (it survives every later navigation on its own).
    if (!scripted.has(page)) {
      scripted.add(page);
      await page.addInitScript(QUEUE_RESIZE_OBSERVER);
    }
    const id = await serve.newSession();
    await page.goto(`${serve.url}/#/s/${id}`);
    const c: Ctx = { page, serve, id, shortH: 0 };
    await c.page.locator('#composer').waitFor();
    // A ResizeObserver always calls back once on its first observe(), for
    // the size it started at: drain that before the walk begins, so it
    // starts genuinely settled, as the spec's Init has it.
    await page.evaluate(() => (window as unknown as { __fireResizeObservers: () => void }).__fireResizeObservers());
    c.shortH = (await measure(c)).live;
    return c;
  },

  actions: {
    AddAttachment: (c) => drop(c),
    // Editing the tag back out is the same as a person deleting it: the
    // draft's own onChange runs the textarea's layout effect straight
    // back down, exactly as attaching grew it.
    RemoveAttachment: (c) => c.page.locator('#composer').fill(''),
    // The real callback the queue holds, run now instead of on the
    // browser's own (far too fast to ever catch mid-flight) schedule.
    async FireObserver(c) {
      await c.page.evaluate(() => (window as unknown as { __fireResizeObservers: () => void }).__fireResizeObservers());
    },
  },

  read: readUiState,
  status: wrap,
  sessions: (c) => [c.id],
});
