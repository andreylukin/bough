// go/tests/model/specs/select_component_reposition_race.fizz in the
// browser: select.tsx's dropdown (the settings popover's model/effort
// Select) at phone width, raced against a resize (a keyboard opening
// under its searchable field) and app.tsx's pane switch, which unmounts
// Settings — and with it the Select — out from under an open picker.
//
// Every field is the page's own client state (which overlay is open,
// the popover's abstract fit, where focus sits): no API reports any of
// it, so, as in go/tests/model/mbt/select_component_reposition_race_test.go,
// "fit" is derived the same way the adapter derives it — from whether
// the picker is open and whether the (faked) keyboard is up — not
// probed from real geometry.
//
// The on-screen keyboard is faked exactly as narrow_layout.spec.ts fakes
// it: window.visualViewport is replaced by one the walk can shrink, and
// __osk exposes the control readUiState and Resize use.
import type { Page } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

// Phone width: the breakpoint that gives the thread pane its own Back
// button (app.tsx's phone layout) instead of a sidebar beside it.
test.use({ viewport: { width: 390, height: 844 } });

// An iPhone keyboard's share of an 844px screen.
const KB = 320;

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  keyboardOn: boolean; // the walk's own last-set state, toggled by Resize
}

function phone(): void {
  const w = window as unknown as Record<string, unknown>;
  let kb = 0;
  const vv = new EventTarget();
  Object.defineProperties(vv, {
    width: { get: () => innerWidth },
    height: { get: () => innerHeight - kb },
    offsetTop: { get: () => 0 },
    offsetLeft: { get: () => 0 },
    pageTop: { get: () => scrollY },
    pageLeft: { get: () => scrollX },
    scale: { get: () => 1 },
  });
  Object.defineProperty(window, 'visualViewport', { value: vv, configurable: true });
  const set = (px: number) => { if (kb !== px) { kb = px; vv.dispatchEvent(new Event('resize')); } };
  w.__osk = { set, kb: () => kb };
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const route = (await page.locator('.app').getAttribute('data-pane')) === 'thread' ? 'thread' : 'list';
  const settings = await page.locator('.head-pop').isVisible();
  const picker = settings && (await page.locator('.head-pop .sel-pop').isVisible());
  const keyboard = await page.evaluate(() => (window as unknown as { __osk: { kb(): number } }).__osk.kb() > 0);
  const fit = picker ? (keyboard ? 'short' : 'full') : '';
  // app.tsx moves focus into the just-opened `.head-pop` dialog itself
  // (its own layout effect), not onto the header's toggle button — so
  // "settings" is anything focused inside .head-pop, "picker" narrows
  // that to the open popover's own field, and everything else (the
  // toggle button included, whether closing gave it focus back or the
  // dialog unmounting dropped focus to <body>) is "page".
  const focus = await page.evaluate(() => {
    const ae = document.activeElement;
    if (!ae || !ae.closest('.head-pop')) return 'page';
    return ae.closest('.sel-pop') || ae.classList.contains('sel-search') ? 'picker' : 'settings';
  });
  return { route, settings, picker, keyboard, fit, focus };
}

const visible = (c: Ctx, sel: string) => c.page.locator(`${sel}:visible`).first();

modelTests<Ctx>({
  spec: 'select_component_reposition_race',
  role: 'App#0',
  shared: true,
  reset: true,

  async init(page, serve) {
    const c: Ctx = { page, serve, id: await serve.newSession(), keyboardOn: false };
    await page.addInitScript(phone);
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await page.locator('button.more[aria-label="Session settings"]').waitFor();
    return c;
  },

  actions: {
    // The sliders button: Settings mounts, nothing else moves yet.
    OpenSettings: async (c) => { await visible(c, 'button.more[aria-label="Session settings"]').click(); },

    // The same toggle button closes it (an outside tap or Escape would
    // too; the spec only ever exercises this while the picker is shut).
    CloseSettings: async (c) => { await visible(c, 'button.more[aria-label="Session settings"]').click(); },

    // The settings dialog's model/effort Select: opening it runs
    // place() against the viewport as it is right now, and its
    // searchable field autofocuses (select.tsx's `show()`).
    async OpenPicker(c) {
      await c.page.locator('.head-pop').getByRole('combobox', { name: /^Next turn model/ }).click();
      await c.page.locator('.head-pop .sel-search').waitFor();
    },

    // Escape: select.tsx's hide(true), closing the popover and
    // refocusing the still-mounted button. The fake keyboard is the
    // picker's own field's doing, so it goes down with the picker (the
    // spec's ClosePicker also clears self.keyboard).
    async ClosePicker(c) {
      await c.page.locator('.head-pop .sel-search').press('Escape');
      c.keyboardOn = false;
      await c.page.evaluate(() => (window as unknown as { __osk: { set(px: number): void } }).__osk.set(0));
    },

    // The keyboard opening or shutting while the picker is up: the
    // resize listener fires and place() recomputes the fit.
    async Resize(c) {
      c.keyboardOn = !c.keyboardOn;
      await c.page.evaluate((px) => (window as unknown as { __osk: { set(px: number): void } }).__osk.set(px), c.keyboardOn ? KB : 0);
    },

    // The phone breakpoint's Back: unmounts Settings and, with it, the
    // Select, even mid-open — the keyboard goes down with it too, since
    // nothing is left to have raised it.
    async PaneSwitch(c) {
      await visible(c, 'button.back').click();
      c.keyboardOn = false;
      await c.page.evaluate(() => (window as unknown as { __osk: { set(px: number): void } }).__osk.set(0));
    },

    // Back to the thread pane, opening the same session again.
    ReturnToThread: async (c) => { await visible(c, `button.row[data-id="${c.id}"]`).click(); },
  },

  read: readUiState,
  status: (c) => c.page.locator('.app'),
  sessions: (c) => [c.id],
});
