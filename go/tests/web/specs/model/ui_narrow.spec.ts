// go/tests/model/specs/ui_narrow.fizz in the browser: the control room
// on a 390px phone, one session, walked path by path with real taps and
// keys against a serve per walk whose model is llm-control. At every
// node the Phone#0 state read off the page must be the spec's, and the
// screen owes what every screen does (helpers/ui-invariants.ts): no
// clipped header or status text, a ring on whatever holds keyboard
// focus, and axe clean on whatever is up. Recipe: go/tests/model/README.md.
//
// The server's steps are held so the page sits where the path says:
//   GET /api/sessions/<id>          the transcript read: held until Loaded
//                                   (let through) or LoadFail (500)
//   POST /api/sessions/<id>/prompt  a send: held until Accept, which queues
//                                   the turn it starts (a "block" turn,
//                                   held until Finish) and lets it through
//
// A phone's on-screen keyboard is an init script (as in
// narrow_layout.spec.ts): window.visualViewport is replaced by one that
// shrinks by KB whenever focus is in something that takes typing and
// grows back when it leaves, as the OS does. `keyboard` is read off the
// page's own --app-height; `fit` off the open layer's box against the
// visual viewport as it is now.
//
// Fields the page draws only where the session is on screen are read
// from what the page itself holds elsewhere: `status` and `archived` off
// the list's row (in the DOM while the list pane is hidden), `draft` off
// the page's saved draft when the composer is not mounted, and `pending`
// off the page's own request still out (a send the page made and serve
// has not answered), which is what "sending" means on any route.
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';
import { uiInvariants } from '../../helpers/ui-invariants';
import * as fs from 'fs';
import * as path from 'path';

// An iPhone keyboard's share of an 844px screen.
const KB = 320;

test.use({ viewport: { width: 390, height: 844 } });

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;       // llm-control's dir
  id: string;        // the session
  title: string;     // its title, unique to the walk
  n: number;         // turn names
  loads: Route[];    // held transcript reads
  sends: Route[];    // held prompt POSTs
  turn: string;      // the running block turn, '' when none
  queued: string[];  // turns queued and not yet taken or released
}

const next = (c: Ctx, p: string) => `${p}${String(++c.n).padStart(4, '0')}`;

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 20_000): Promise<void> {
  for (const end = Date.now() + ms; !(await ok()); await new Promise((r) => setTimeout(r, 25))) {
    if (Date.now() > end) throw new Error(`ui_narrow: ${what} after ${ms}ms`);
  }
}

interface Row { id: string; status: string; live?: boolean; archived?: boolean }
async function row(c: Ctx, id = c.id): Promise<Row | undefined> {
  const r = await c.serve.api.get(`/api/sessions/${id}`);
  return r.ok() ? (await r.json()).session as Row : undefined;
}

// ---- the phone ----

const phone = (kb: number) => {
  const w = window as unknown as Record<string, unknown>;
  let up = 0;
  const vv = new EventTarget();
  Object.defineProperties(vv, {
    width: { get: () => innerWidth },
    height: { get: () => innerHeight - up },
    offsetTop: { get: () => 0 },
    offsetLeft: { get: () => 0 },
    pageTop: { get: () => scrollY },
    pageLeft: { get: () => scrollX },
    scale: { get: () => 1 },
  });
  Object.defineProperty(window, 'visualViewport', { value: vv, configurable: true });
  const set = (px: number) => { if (up !== px) { up = px; vv.dispatchEvent(new Event('resize')); } };
  const typing = (el: Element | null) => Boolean(el && ((el as HTMLElement).isContentEditable || el.tagName === 'TEXTAREA' ||
    (el.tagName === 'INPUT' && /^(text|search|email|url|tel|password|)$/.test((el as HTMLInputElement).type))));
  // The OS moves the keyboard once a tap is over: moving it at the
  // pointerdown that took focus shifted the page under the finger, and
  // the click landed on whatever slid under it. A MessageChannel is not
  // on the page's (fake) clock and runs after the tap's click.
  let tapping = false;
  const ch = new MessageChannel();
  const follow = () => { if (!tapping) set(typing(document.activeElement) ? kb : 0); };
  ch.port1.onmessage = () => { tapping = false; follow(); };
  document.addEventListener('pointerdown', () => { tapping = true; }, true);
  document.addEventListener('pointerup', () => ch.port2.postMessage(0), true);
  document.addEventListener('focusin', follow, true);
  document.addEventListener('focusout', () => queueMicrotask(follow), true);
  new MutationObserver(() => { if (up && !tapping && !typing(document.activeElement)) set(0); }).observe(document, { childList: true, subtree: true });
  w.__osk = { kb: () => up };
};

// ---- the page ----

const visible = (c: Ctx, sel: string) => c.page.locator(`${sel}:visible`).first();
const sRow = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const composer = (c: Ctx) => c.page.locator('#composer');
const pal = (c: Ctx) => c.page.locator('.pal');
const pop = (c: Ctx) => c.page.locator('.head-pop');
const details = (c: Ctx) => c.page.locator('details.rt-more');
const sheet = (c: Ctx) => c.page.locator('.work-sheet');
const dlg = (c: Ctx) => c.page.locator('.dlg[aria-modal="true"]');

// The top layer's element, the one whose box must fit the viewport.
const LAYERS: [string, string][] = [
  ['picker', '.head-pop .sel-open .sel-pop'],
  ['settings', '.head-pop'],
  ['details', 'details.rt-more[open] > .rt-pop'],
  ['work', '.work-sheet'],
  ['palette', '.pal'],
  ['dialog', '.dlg[aria-modal="true"]'],
];

// The spec's words for the page's: its "done" is any word for a session
// that is not running, Done or Stopped.
const HEAD: Record<string, string> = { Sending: 'sending', Waiting: 'working', Working: 'working', Done: 'done', Stopped: 'done' };

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const hash = await p.evaluate(() => location.hash);
  const route = ['', '#', '#/'].includes(hash) ? 'list' : hash === `#/s/${c.id}` ? 'thread' : hash === '#/hooks' ? 'hooks' : `unknown: ${hash}`;
  const dom = await p.evaluate(({ id, layers }) => {
    const shown = (el: Element | null) => Boolean(el && (el as HTMLElement).getClientRects().length && getComputedStyle(el).visibility !== 'hidden');
    const q = (s: string) => document.querySelector(s);
    const a = document.activeElement;
    const inside = (s: string) => Boolean(a && a.closest(s));
    const focus = inside('.head-pop .sel-open') ? 'picker'
      : inside('.head-pop') ? 'settings'
      // Escape gives focus back to the summary: a strip control, "page".
      : inside('details.rt-more[open]') ? 'details'
      : inside('.work-sheet') ? 'work'
      : inside('.pal') ? 'palette'
      : inside('.dlg') ? 'dialog'
      : a?.id === 'composer' ? 'composer' : 'page';
    const appHeight = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--app-height'));
    const keyboard = appHeight < innerHeight - 1;
    // Every layer up must lie inside the visual viewport as it is now.
    let fit = '';
    const vv = window.visualViewport!;
    for (const [, sel] of layers) {
      const el = q(sel);
      if (!shown(el)) continue;
      const r = el!.getBoundingClientRect();
      const inside = r.top >= vv.offsetTop - 0.5 && r.bottom <= vv.offsetTop + vv.height + 0.5;
      const word = keyboard ? 'short' : 'full';
      fit = inside ? word : `${word}, but ${sel} spans ${Math.round(r.top)}..${Math.round(r.bottom)} of ${Math.round(vv.height)}`;
      break;
    }
    let saved = '';
    try { saved = localStorage.getItem('bough:draft:' + id) ?? ''; } catch { saved = ''; }
    const btn = q('.composer button.btn-primary') as HTMLButtonElement | null;
    return {
      focus, keyboard, fit, saved,
      back: [...document.querySelectorAll('button.back')].some(shown),
      nav: shown(q('.phone-nav')),
      loadingLine: [...document.querySelectorAll('.transcript-state[role="status"]')].some((e) => shown(e) && /Loading transcript/.test(e.textContent ?? '')),
      errorNote: [...document.querySelectorAll('.transcript .error-note, .transcript .transcript-state')].some((e) => shown(e) && /Couldn’t load|taking too long/.test(e.textContent ?? '')),
      // The words on screen: a bare StatusMark's label is for screen readers only.
      head: (() => {
        const el = q('.thread-head .status');
        if (!shown(el)) return '';
        const copy = el!.cloneNode(true) as HTMLElement;
        copy.querySelectorAll('.visually-hidden, [aria-hidden="true"]').forEach((n) => n.remove());
        return (copy.textContent ?? '').trim();
      })(),
      composerUp: shown(q('#composer')),
      draft: (q('#composer') as HTMLTextAreaElement | null)?.value ?? '',
      send: btn && shown(btn) && !btn.disabled ? (btn.getAttribute('aria-label') ?? '').toLowerCase() : 'off',
      stop: shown(q('button.composer-stop')),
      settings: shown(q('.head-pop')),
      picker: shown(q('.head-pop .sel-open .sel-pop')),
      details: Boolean((q('details.rt-more') as HTMLDetailsElement | null)?.open) && shown(q('details.rt-more')),
      work: shown(q('.work-sheet')),
      palette: shown(q('.pal')),
      dialog: shown(q('.dlg[aria-modal="true"]')),
    };
  }, { id: c.id, layers: LAYERS });

  const label = (await sRow(c).getAttribute('aria-label').catch(() => null)) ?? '';
  const word = label.split(', ')[1] ?? '';
  const status = word === 'Running' ? 'running' : word === 'Done' || word === 'Stopped' ? 'idle' : `unknown row: ${label}`;
  const archived = (await c.page.locator(`#sec-archived button.row[data-id="${c.id}"]`).count()) > 0;

  let transcript = 'none', shown = '';
  if (route === 'thread') {
    transcript = dom.loadingLine ? 'loading' : dom.errorNote ? 'error' : dom.head ? 'loaded' : 'blank';
    shown = transcript === 'loaded' ? (HEAD[dom.head] ?? `head: ${dom.head}`) : transcript;
  } else if (route === 'list') {
    shown = await sRow(c).isVisible() ? (word === 'Running' ? 'running' : word === 'Done' || word === 'Stopped' ? 'done' : `row: ${word}`) : 'no row';
  }
  const draft = dom.composerUp ? dom.draft.trim() !== '' : dom.saved.trim() !== '';
  return {
    route,
    back: dom.back,
    nav: dom.nav,
    transcript,
    status,
    archived,
    pending: c.sends.length > 0,
    draft,
    settings: dom.settings,
    picker: dom.picker,
    details: dom.details,
    work: dom.work,
    palette: dom.palette,
    dialog: dom.dialog,
    focus: dom.focus,
    keyboard: dom.keyboard,
    fit: dom.fit,
    shown,
    send: route === 'thread' ? dom.send : 'off',
    stop: route === 'thread' && dom.stop,
  };
}

// ---- the server's side ----

async function held(what: string, list: Route[]): Promise<Route[]> {
  await until(`no ${what} from the page`, () => list.length > 0);
  return list.splice(0);
}
const go = (r: Route) => r.continue().catch(() => {});

// A thread that goes away leaves its read unanswered; the next Open reads anew.
async function dropLoads(c: Ctx): Promise<void> {
  for (const r of c.loads.splice(0)) await go(r);
}

// Turns queued for a turn that will not come (Stop ended it) would be
// taken by the next send's turn instead of its own.
function unqueue(c: Ctx): void {
  for (const name of c.queued.splice(0)) {
    if (!fs.existsSync(path.join(c.dir, name + '.taken'))) fs.rmSync(path.join(c.dir, name + '.json'), { force: true });
  }
}

// ---- Init: one session, one finished turn with usage, one finished
// background agent (the Work button), shown on the list ----

async function init(page: Page, serve: Serve): Promise<Ctx> {
  test.info().setTimeout(600_000);
  const c: Ctx = { page, serve, dir: controlDir(serve.home), id: '', title: '', n: 0, loads: [], sends: [], turn: '', queued: [] };
  c.title = `narrow ${Date.now().toString(36).slice(-5)}`;
  const first = next(c, 't');
  queue(c.dir, first, { mode: 'ok', text: 'first turn done' });
  c.id = await serve.newSession(c.title);
  await waitTaken(c.dir, first);
  await until('the first turn done', async () => (await row(c))?.status === 'done');

  // A background agent, as tools.spawn({background}) asks serve for one
  // (plugins/workers/background.go). Its report wakes the session for a
  // turn of its own, answered and finished before the walk starts.
  const kid = next(c, 't'), wake = next(c, 't');
  queue(c.dir, kid, { mode: 'ok', text: 'agent done' });
  queue(c.dir, wake, { mode: 'ok', text: 'noted the agent' });
  const res = await serve.api.post('/api/sessions', { data: { cwd: serve.work, prompt: 'side job', spawnedBy: c.id } });
  if (!res.ok()) throw new Error(`ui_narrow: spawn: ${res.status()} ${await res.text()}`);
  const kidId = (await res.json()).session.id as string;
  await waitTaken(c.dir, kid);
  await until('the agent done', async () => (await row(c, kidId))?.status === 'done');
  await waitTaken(c.dir, wake);
  await until('the wake turn done', async () => (await row(c))?.status === 'done');

  await page.addInitScript(phone, KB);
  await page.route((u) => u.pathname === `/api/sessions/${c.id}` && !u.searchParams.has('since'), (r) => {
    if (r.request().method() !== 'GET') return r.continue();
    c.loads.push(r);
  });
  await page.route((u) => u.pathname === `/api/sessions/${c.id}/prompt`, (r) => { c.sends.push(r); });
  await page.goto(`${serve.url}/#/`);
  await sRow(c).waitFor();
  // The Archived section open, so the row stays on the list once the
  // walk archives it (the spec's list always shows the one session).
  await visible(c, 'button.sec-fold[aria-controls="sec-archived"]').click();
  return c;
}

// Opens the session from anywhere its row is: the list's row.
async function openRow(c: Ctx): Promise<void> {
  await visible(c, `button.row[data-id="${c.id}"]`).click();
}

modelTests<Ctx>({
  spec: 'ui_narrow',
  role: 'Phone#0',
  config: CONTROL_CONFIG,
  init,

  actions: {
    Open: openRow,
    // The header's Back; with the Work sheet up (modal, over the header)
    // the phone's own back, the edge swipe.
    async Back(c) {
      if (await sheet(c).isVisible()) await c.page.goBack();
      else await visible(c, 'button.back').click();
      await dropLoads(c);
    },
    GoHooks: (c) => visible(c, '.phone-nav a[href="#/hooks"]').click(),
    NewSession: (c) => visible(c, 'button.side-new').click(),
    ClosePalette: (c) => c.page.keyboard.press('Escape'),
    // The New session palette offers places to start, not sessions: the
    // session is reached from the same palette by searching (⌘K turns it
    // into "Search sessions or run a command…") and picking the match.
    async PalettePick(c) {
      await c.page.keyboard.press('ControlOrMeta+k');
      await c.page.locator('.pal-field').fill(c.title);
      await pal(c).locator(`[id="pal-s:${c.id}"]`).waitFor();
      await pal(c).locator(`[id="pal-s:${c.id}"]`).click();
    },

    async Loaded(c) {
      for (const r of await held('transcript read', c.loads)) await go(r);
    },
    async LoadFail(c) {
      for (const r of await held('transcript read', c.loads)) {
        await r.fulfill({ status: 500, contentType: 'application/json', body: '{"error":"ui_narrow: transcript read failed on purpose"}' }).catch(() => {});
      }
    },
    RetryLoad: (c) => visible(c, '.transcript button:has-text("Retry")').click(),

    TapComposer: (c) => composer(c).click(),
    // The keyboard's Done: the field lets go of focus.
    HideKeyboard: (c) => c.page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur()),
    Type: (c) => composer(c).pressSequentially('next step'),
    Send: (c) => composer(c).press('Enter'),

    // Serve takes the send: a send starts a turn (held running until
    // Finish); a steer joins the running one, whose model then answers it
    // with a turn of its own once released.
    async Accept(c) {
      const [r] = await held('send', c.sends);
      const name = next(c, 't');
      if (c.turn) {
        queue(c.dir, name, { mode: 'ok', text: `steered ${name}` });
        c.queued.push(name);
      } else {
        queue(c.dir, name, { mode: 'block', text: `finished ${name}` });
        c.turn = name;
      }
      const res = await r.fetch();
      if (!res.ok()) throw new Error(`ui_narrow: send: ${res.status()} ${await res.text()}`);
      await r.fulfill({ response: res });
      if (c.turn === name) {
        await waitTaken(c.dir, name);
        await until('the turn running', async () => (await row(c))?.status === 'running');
      }
    },
    async Finish(c) {
      release(c.dir, c.turn);
      c.turn = '';
      await until('the turn done', async () => (await row(c))?.status === 'done');
      // Steers the model answered in one call leave their other turns
      // queued, where the next send's turn would be taken after them.
      unqueue(c);
    },
    async Stop(c) {
      await visible(c, 'button.composer-stop').click();
      await until('the turn stopped', async () => (await row(c))?.status !== 'running');
      release(c.dir, c.turn);
      c.turn = '';
      unqueue(c);
    },

    OpenSettings: (c) => visible(c, 'button.more[aria-label="Session settings"]').click(),
    CloseSettings: (c) => c.page.keyboard.press('Escape'),
    OpenPicker: (c) => pop(c).getByRole('combobox', { name: /^Next turn model/ }).click(),
    ClosePicker: (c) => c.page.keyboard.press('Escape'),
    Archive: (c) => pop(c).getByRole('button', { name: 'Archive…' }).click(),
    CancelArchive: (c) => dlg(c).getByRole('button', { name: 'Cancel' }).click(),
    async ConfirmArchive(c) {
      const done = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/archive`) && r.request().method() === 'POST');
      await dlg(c).getByRole('button', { name: 'Archive', exact: true }).click();
      const res = await done;
      if (!res.ok()) throw new Error(`ui_narrow: archive: ${res.status()}`);
    },
    OpenDetails: (c) => visible(c, 'details.rt-more > summary').click(),
    CloseDetails: (c) => c.page.keyboard.press('Escape'),
    OpenWork: (c) => visible(c, 'button.work-summary').click(),
    CloseWork: (c) => sheet(c).locator('.work-close').click(),
  },

  // LoadFail's 500: Chromium logs the status; the page's answer is the ErrorNote.
  allowConsole: /^Failed to load resource: the server responded with a status of 500 /,

  read: readUiState,
  // What carries the state: the session's row on the list, the thread's
  // header in the conversation, the page's own header elsewhere.
  status: (c) => c.page.locator('.app[data-pane="list"] .sidebar, .app[data-pane="thread"] .app-main .thread-head').first(),
  async invariants(c, where) {
    // Layers fade in: axe measuring contrast mid-fade reads the text as
    // fainter than it is. Finite animations only (spinners loop).
    await c.page.evaluate(() => Promise.all(document.getAnimations()
      .filter((a) => Number.isFinite(Number(a.effect?.getComputedTiming().endTime)))
      .map((a) => a.finished.catch(() => undefined))));
    await uiInvariants(c.page, {
      include: ['.thread-head', '.composer', '.transcript-state', '.head-pop', 'details.rt-more[open]', '.work-sheet', '.pal', '.dlg', '.phone-nav', '.app[data-pane="list"] .sidebar'],
      text: '.thread-head h1, .thread-head .status, .transcript-state, .side-title, .head-pop-item, .work-title, .dlg-title, .pal-label, .pal-hint, .send-word, .rt-label, .rt-value',
    }, where);
  },
  // The spec is the page's state (layers, focus, keyboard), which no
  // transcript records: nothing for the history trace check to replay.
  sessions: () => [],
  async cleanup(c) {
    for (const r of [...c.loads.splice(0), ...c.sends.splice(0)]) await go(r);
    if (c.turn) release(c.dir, c.turn);
  },
});
