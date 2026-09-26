// go/tests/model/specs/call_row_hover_popover_race.fizz in the browser
// (recipe: go/tests/model/README.md): useThinPop's show/hide timers for
// two adjacent call rows (app.tsx, ~2041-2119). The Go twin, which
// reimplements the same enter/leave/ShowFire/HideFire logic against a
// real serve with no real timers involved, is
// go/tests/model/mbt/call_row_hover_popover_race_test.go.
//
// Two turns on one session each run one code-mode block with a single
// tools.bash call, so the transcript ends with two adjacent, ungrouped
// ToolCall rows (each turn's own block never reaches the >=2-calls
// threshold that wraps rows in a collapsed ToolGroup). From there the
// walk only hovers and moves the mouse, and fires the page's own show
// (300ms) / hide (150ms) timers by advancing Playwright's fake clock.
//
// hovered and visible are read straight off the DOM (:hover and each
// row's aria-describedby, which useThinPop only sets on the row whose
// popover is the one rendered). pendingShow/pendingHide are not: unlike
// visible, a still-pending timer leaves no trace on the page, so — like
// "inflight" in ask_beside_parallel_calls.spec.ts — they are the walk's
// own bookkeeping, kept in lockstep with the spec's enter/leave/ShowFire/
// HideFire. Each row's REAL 300/150ms timer is tracked separately
// (dueShow/dueHide by row) because the two rows are two independent
// React components with their own timer ref; the fake clock is only
// ever advanced to the deadline the action names, on the row the spec's
// single pendingShow/pendingHide slot currently names. Advancing past an
// earlier, unrelated row's own due timer along the way is real and
// harmless — that row's popover was already showing nothing new either
// way — and is exactly why the hide timer being shorter than the show
// timer keeps AtMostOneVisible true in the first place.
import { CONTROL_CONFIG, controlDir, queue, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Page } from '@playwright/test';
import type { Serve } from '../../helpers/serve';

type Row = 0 | 1;
type RowOrNone = Row | 'none';

const SHOW_MS = 300;
const HIDE_MS = 150;

interface Ctx {
  page: Page;
  id: string;
  now: number; // ms advanced on the page's fake clock so far
  dueShow: Partial<Record<Row, number>>; // each row's own real show deadline, if armed
  dueHide: Partial<Record<Row, number>>; // each row's own real hide deadline, if armed
  pendingShow: RowOrNone; // the spec's single slot: not observable on the page
  pendingHide: RowOrNone;
}

async function untilDone(serve: Serve, id: string): Promise<void> {
  const deadline = Date.now() + 20_000;
  for (;;) {
    const res = await serve.api.get(`/api/sessions/${id}`);
    if (!res.ok()) throw new Error(`session status: ${res.status()} ${await res.text()}`);
    if ((await res.json()).session.status !== 'running') return;
    if (Date.now() > deadline) throw new Error('turn did not finish');
    await new Promise((r) => setTimeout(r, 50));
  }
}

const rowLoc = (c: Ctx, row: Row) => c.page.locator('.transcript details.toolcall > summary').nth(row);

async function dom(c: Ctx): Promise<{ hovered: RowOrNone; visible: RowOrNone }> {
  return c.page.evaluate(() => {
    const rows = [...document.querySelectorAll<HTMLElement>('.transcript details.toolcall > summary')];
    const row = (i: number): 0 | 1 | 'none' => (i === 0 ? 0 : i === 1 ? 1 : 'none');
    return {
      hovered: row(rows.findIndex((el) => el.matches(':hover'))),
      // useThinPop only sets aria-describedby on the row its own popover
      // (a portal into document.body) is currently rendered for.
      visible: row(rows.findIndex((el) => el.hasAttribute('aria-describedby'))),
    };
  });
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const d = await dom(c);
  return { hovered: d.hovered, pendingShow: c.pendingShow, pendingHide: c.pendingHide, visible: d.visible };
}

// Advances the fake clock to due (a no-op once something else already
// carried "now" past it, e.g. a still-outstanding timer on the other row).
async function advanceTo(c: Ctx, due: number): Promise<void> {
  const delta = due - c.now;
  if (delta <= 0) return;
  await c.page.clock.fastForward(delta);
  c.now = due;
}

async function enter(c: Ctx, row: Row): Promise<void> {
  await rowLoc(c, row).hover();
  // The real handler (show) unconditionally clearTimeouts the row's own
  // ref before arming a fresh one, cancelling whatever it held.
  delete c.dueHide[row];
  if (c.pendingHide === row) c.pendingHide = 'none';
  c.pendingShow = row;
  c.dueShow[row] = c.now + SHOW_MS;
}

async function leave(c: Ctx, row: Row): Promise<void> {
  await c.page.mouse.move(2, 2);
  delete c.dueShow[row];
  if (c.pendingShow === row) c.pendingShow = 'none';
  // Only arm a hide if that row's popover is the one actually on screen.
  const { visible } = await dom(c);
  if (visible === row) {
    c.pendingHide = row;
    c.dueHide[row] = c.now + HIDE_MS;
  }
}

async function fireShow(c: Ctx): Promise<void> {
  const row = c.pendingShow;
  if (row === 'none') throw new Error('ShowFire: no show timer pending');
  await advanceTo(c, c.dueShow[row]!);
  delete c.dueShow[row];
  c.pendingShow = 'none';
}

async function fireHide(c: Ctx): Promise<void> {
  const row = c.pendingHide;
  if (row === 'none') throw new Error('HideFire: no hide timer pending');
  await advanceTo(c, c.dueHide[row]!);
  delete c.dueHide[row];
  c.pendingHide = 'none';
}

modelTests<Ctx>({
  spec: 'call_row_hover_popover_race',
  role: 'Popover#0',
  config: CONTROL_CONFIG + '- id: loop\n  plugin: loop\n',
  // The walk only hovers and fires timers: reads must not also run the
  // page's own polls forward, or a read would fire a timer the trace has
  // not named yet.
  pollStepMs: 0,

  async init(page, serve) {
    const dir = controlDir(serve.home);
    queue(dir, 't0001', { mode: 'ok', text: '```js\ntools.bash("echo rowzero")\n```' });
    const id = await serve.newSession('start rowzero');
    await waitTaken(dir, 't0001');
    await untilDone(serve, id);
    queue(dir, 't0002', { mode: 'ok', text: '```js\ntools.bash("echo rowone")\n```' });
    const res = await serve.api.post(`/api/sessions/${id}/prompt`, { data: { text: 'start rowone' } });
    if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
    await waitTaken(dir, 't0002');
    await untilDone(serve, id);
    await page.goto(`${serve.url}/#/s/${id}`);
    await page.locator('.transcript details.toolcall').nth(1).waitFor();
    return { page, id, now: 0, dueShow: {}, dueHide: {}, pendingShow: 'none', pendingHide: 'none' };
  },

  actions: {
    Enter0: (c) => enter(c, 0),
    Leave0: (c) => leave(c, 0),
    Enter1: (c) => enter(c, 1),
    Leave1: (c) => leave(c, 1),
    ShowFire: fireShow,
    HideFire: fireHide,
  },

  read: readUiState,
  status: (c) => rowLoc(c, 0),
  sessions: () => [], // nothing here is a server action; no history to replay
});
