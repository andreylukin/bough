// go/tests/model/specs/sidebar_closed_notice_reopen_race.fizz in the
// browser: the sidebar's own collapse toggle (Close/Open, the person's
// own clicks on .side-collapse) racing a finish notice's unconditional
// force-reopen (Notice). Recipe: go/tests/model/README.md.
//
// The world the spec names: one session, created idle so arriving at the
// empty overview never auto-opens it (app.tsx's "arrived" effect only
// fires once, against whatever rows exist at that moment); Notice then
// starts a real turn on it and force-reopens the sidebar the way a
// finish notice does — by revealing the row from the control room's
// overview ("N session(s) running · open", ControlOverview's onReveal),
// never through the composer, since no session is ever open in this
// walk. justNotified has no field of its own to read: the reveal effect
// (app.tsx ~628) is the only path that both force-reopens and moves
// focus onto the row, so a walk's own Close/Open (which focus the
// toggle button instead) and Notice's reveal are told apart by where
// focus lands, exactly as closed is told apart by the toggle's own
// aria-expanded.
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;
  id: string;   // the one session Notice drives turns on
  turn: number;
  held: string; // the llm-control turn name in flight, '' when none
}

async function until(what: string, done: () => boolean | Promise<boolean>, ms = 10_000): Promise<void> {
  for (const end = Date.now() + ms; !(await done()); await new Promise((r) => setTimeout(r, 50))) {
    if (Date.now() > end) throw new Error(`sidebar_closed_notice_reopen_race: ${what} after ${ms}ms`);
  }
}

const collapse = (c: Ctx) => c.page.locator('.side-collapse');

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  return c.page.evaluate((id) => {
    const closed = document.querySelector('.side-collapse')?.getAttribute('aria-expanded') === 'false';
    const active = document.activeElement as HTMLElement | null;
    const justNotified = !!id && !!active && active.matches(`.sidebar button.row[data-id="${id}"]`);
    return { closed, justNotified };
  }, c.id);
}

modelTests<Ctx>({
  spec: 'sidebar_closed_notice_reopen_race',
  role: 'Sidebar#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const c: Ctx = { page, serve, dir: controlDir(serve.home), id: '', turn: 0, held: '' };
    await page.addInitScript(() => { try { localStorage.setItem('bough:welcome-done', '1'); } catch { /* storage off */ } });
    await page.goto(`${serve.url}/#/`);
    // Past the "arrived" effect's one shot: with no rows yet it never
    // auto-opens anything, so the session Notice creates next stays on
    // the overview instead of jumping straight into its thread.
    await page.locator('.ov-empty-title').waitFor();
    c.id = await serve.newSession();
    return c;
  },

  actions: {
    // The toggle is the same button both ways; the spec's own require
    // (not closed / closed) picks which direction a path takes it.
    Close: (c) => collapse(c).click(),
    Open: (c) => collapse(c).click(),

    // A finish notice: a real turn, run to completion on the session
    // Init made, exactly as the Go adapter's own Notice does — then
    // revealed from the overview, the one UI path that force-reopens
    // the sidebar regardless of what Close/Open just did to it. Nothing
    // here is ever the session's own composer: it is never open.
    async Notice(c) {
      c.turn++;
      const name = `t${String(c.turn).padStart(4, '0')}`;
      queue(c.dir, name, { mode: 'block', text: `finished ${name}` });
      c.held = name;
      const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: `turn ${name}` } });
      if (!res.ok()) throw new Error(`sidebar_closed_notice_reopen_race: prompt: ${res.status()} ${await res.text()}`);
      await waitTaken(c.dir, name);

      // The overview's own list poll (4 s, on the page's clock) has to
      // see the session running before its reveal link exists to click.
      const open = c.page.getByRole('button', { name: 'open' });
      await until('the overview never showed the running session', async () => {
        await c.page.clock.fastForward(4_000);
        return open.isVisible();
      });
      await open.click();

      release(c.dir, name);
      c.held = '';
      await until('the turn never finished', async () =>
        (await (await c.serve.api.get(`/api/sessions/${c.id}`)).json()).session.status === 'done');
    },
  },

  read: readUiState,
  status: collapse,
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.held) release(c.dir, c.held);
  },
});
