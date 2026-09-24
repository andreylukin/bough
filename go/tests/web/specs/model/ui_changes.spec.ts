// go/tests/model/specs/ui_changes.fizz in the browser: the changes view
// as a person meets it, the header chip's popover on a wide window, the
// chip link on a phone, the full page, the palette over them. Every
// generated path is walked with real clicks and keys against a serve
// shared by the worker (llm-control is the model), and at EVERY node:
//
//   - readUiState equals the spec's role state (read off the DOM only),
//   - the page does not scroll sideways,
//   - no text in the header or the scope tabs is clipped,
//   - nothing was logged as an error,
//   - the focused element, when focus is visible, draws a ring,
//   - axe finds nothing on the surface on screen.
//
// The walk is written out here rather than through modelTests because
// the last three checks are this flow's; loadPaths and roleState are
// the shared helpers.
//
// The spec's reads answer in steps of their own, so the page's requests
// are held in the browser and each server step lets through what it
// names:
//
//   - edits and changes reads (the session's edits, the working tree)
//     are held from the moment they are sent. Answer, Retry, Poll and
//     AgentEdit let them through until the page goes quiet;
//     AnswerFails and PollFails fail them.
//   - diff reads are held until DiffAnswer. A page card that re-fetches
//     keeps the patch it has, so its held re-fetch changes nothing.
//
// The page's clock is paused (the 10 s tree poll runs when Poll or
// PollFails moves it) and moved a few frames before every read, so a
// poll never lands inside a step that did not ask for one.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import AxeBuilder from '@axe-core/playwright';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, type Turn } from '../../helpers/control';
import { loadPaths, roleState } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

const SPEC = 'ui_changes';
const ROLE = 'Changes#0';
const WIDE = { width: 1100, height: 700 };
const PHONE = { width: 600, height: 700 };

test.use({ workerServeOpts: { config: CONTROL_CONFIG } });
// Up to eleven steps, one of them maybe a real turn, each with an axe pass.
test.describe.configure({ timeout: 120_000 });

type Kind = 'edits' | 'changes' | 'diff';
interface Held { route: Route; kind: Kind; gen: number }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;
  edited: boolean;  // the world: the agent has written a and b
  held: Held[];
  pass: Set<Kind>;  // kinds let through for now
  inflight: number; // let through, not answered yet
  gen: number;      // one per surface that mounts its own reads
  back: string;     // what had focus when the palette opened, read off the DOM then
  scope: string;    // the chip's scope, which a phone does not show
  turns: string[];  // queued llm-control turns, removed if left untaken
}

// One queue per serve and one serve per worker: turn names only grow.
let turnSeq = 0;

const READ = /\/api\/sessions\/[^/]+\/(edits|changes|diff)(\?|$)/;
const kindOf = (url: string): Kind => (READ.exec(url)?.[1] as Kind) ?? 'diff';

function git(c: Ctx, ...args: string[]): void {
  execFileSync('git', ['-C', c.dir, '-c', 'user.name=model', '-c', 'user.email=model@test', '-c', 'commit.gpgsign=false', ...args], {
    env: { ...process.env, HOME: c.serve.home, GIT_CONFIG_NOSYSTEM: '1' },
    stdio: 'pipe',
  });
}

// A checkout whose "h" was already dirty before the session: the tree
// always lists it, the session's edits never do.
function checkout(c: Ctx): void {
  git(c, 'init', '-q', '-b', 'main');
  fs.writeFileSync(path.join(c.dir, 'h'), 'one\n');
  git(c, 'add', 'h');
  git(c, 'commit', '-q', '-m', 'base');
  fs.writeFileSync(path.join(c.dir, 'h'), 'one\ntwo\n');
}

// --- the reads the walk holds

function onRead(c: Ctx, route: Route): void {
  const kind = kindOf(route.request().url());
  if (c.pass.has(kind)) { go(c, route); return; }
  c.held.push({ route, kind, gen: c.gen });
}

function go(c: Ctx, route: Route): void {
  c.inflight++;
  route.continue().catch(() => {});
}

// A failed read as the page sees it. Not a 5xx: Chromium logs each one
// as a console error; a body that is not JSON fails the same .catch.
function fail(route: Route): void {
  route.fulfill({ status: 200, contentType: 'application/json', body: 'not json' }).catch(() => {});
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// Wait until reads of every kind in kinds, sent at gen or later, are held.
async function waitHeld(c: Ctx, kinds: Kind[], gen = c.gen): Promise<void> {
  const deadline = Date.now() + 10_000;
  const ready = () => kinds.every((k) => c.held.some((h) => h.kind === k && h.gen >= gen));
  while (!ready()) {
    if (Date.now() > deadline) throw new Error(`no held ${kinds.join('+')} read (gen ${gen}); held: ${c.held.map((h) => `${h.kind}@${h.gen}`).join(', ')}`);
    await c.page.clock.runFor(16);
    await sleep(15);
  }
}

// Let kinds through, what is held and what comes, until nothing is in
// flight or newly held for a while: one read's answer can start another.
async function letThrough(c: Ctx, kinds: Kind[], also?: () => Promise<void>): Promise<void> {
  for (const k of kinds) c.pass.add(k);
  try {
    for (const h of c.held.filter((h) => kinds.includes(h.kind))) { c.held.splice(c.held.indexOf(h), 1); go(c, h.route); }
    if (also) await also();
    const deadline = Date.now() + 20_000;
    let quiet = 0;
    while (quiet < 4) {
      if (Date.now() > deadline) throw new Error(`${c.inflight} reads still in flight`);
      await sleep(50);
      quiet = c.inflight > 0 ? 0 : quiet + 1;
    }
  } finally {
    for (const k of kinds) c.pass.delete(k);
  }
}

function failHeld(c: Ctx, kinds: Kind[]): void {
  for (const h of c.held.filter((h) => kinds.includes(h.kind))) { c.held.splice(c.held.indexOf(h), 1); fail(h.route); }
}

// A new surface is about to mount its own reads (the page, or the thread
// again): what the last one sent is dead, and let go.
async function remount(c: Ctx, act: () => Promise<void>): Promise<void> {
  const gen = ++c.gen;
  await act();
  await waitHeld(c, ['edits', 'changes'], gen);
  for (const h of c.held.filter((h) => h.gen < gen && h.kind !== 'diff')) { c.held.splice(c.held.indexOf(h), 1); go(c, h.route); }
}

// --- the page, as a person sees it

const chipDetails = (c: Ctx) => c.page.locator('details.rt-jobs:has(a.chg-full)');
const pop = (c: Ctx) => c.page.locator('details.rt-jobs:has(a.chg-full) .rt-diff');
const onPage = (c: Ctx) => c.page.locator('.chg-page');

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await c.page.evaluate(() => {
    const chip = document.querySelector<HTMLDetailsElement>('details.rt-jobs:has(a.chg-full)');
    const pageEl = document.querySelector('.chg-page');
    const pal = document.querySelector('.pal[role="dialog"]');
    const strip = document.querySelector('.thread-head .runtime-strip');
    const a = document.activeElement;
    const focus = pal && pal.contains(a) ? 'palette' : chip && chip.contains(a) ? 'chip' : 'body';
    // The chip's own words: a summary on a wide window, a link or a
    // span on a phone (folded under "Details", still in the document).
    const label = [...(strip?.querySelectorAll('[aria-label]') ?? [])].map((e) => e.getAttribute('aria-label') ?? '')
      .find((l) => /^(Session edits: |No edits\. )/.test(l)) ?? null;
    const body = pageEl ?? chip?.querySelector('.rt-diff') ?? null;
    const tabs = [...(body?.querySelectorAll('[role="tablist"] [role="tab"]') ?? [])].map((t) => {
      const count = t.querySelector('.seg-count');
      return {
        text: (t.textContent ?? '').trim(),
        selected: t.getAttribute('aria-selected') === 'true',
        count: (count?.textContent ?? '').trim(),
        title: count?.getAttribute('title') ?? '',
      };
    });
    const notes = [...(body?.querySelectorAll('.rt-label') ?? [])].map((e) => (e.textContent ?? '').trim());
    // The shown file: the popover's current list row, or the page's open cards.
    const picks = pageEl
      ? [...pageEl.querySelectorAll<HTMLDetailsElement>('details.chg-card')].filter((d) => d.open).map((d) => d.querySelector('.chg-card-path b')?.textContent ?? '?')
      : [...(body?.querySelectorAll('.chg-files button[aria-current="true"] .mono') ?? [])].map((e) => e.textContent ?? '?');
    const diffs = pageEl
      ? [...pageEl.querySelectorAll<HTMLDetailsElement>('details.chg-card')].filter((d) => d.open)
      : [...(body?.querySelectorAll('.chg-diff') ?? [])];
    const diff = diffs.map((d) => (d.querySelector('pre.rt-diff-body') ? 'ok' : d.querySelector('.pending') ? 'loading' : `other: ${(d.textContent ?? '').trim()}`));
    return {
      wide: window.innerWidth > 720, page: !!pageEl, pop: !!chip?.open, palette: !!pal, focus, label, strip: !!strip,
      mounted: !!body, tabs, notes, picks, diff,
    };
  });

  // The scope a mounted body shows; a phone's chip keeps its own unseen.
  const sel = dom.tabs.find((t) => t.selected)?.text ?? '';
  if (dom.mounted) c.scope = sel.startsWith('Session edits') ? 'session' : sel.startsWith('Working tree') ? 'tree' : `unknown: ${sel}`;

  let sess: string, tree: string;
  if (dom.page) {
    // The page's tabs: a count, "…" loading, "!" failed; the shown tab
    // says stale in words, another in its count's title.
    const status = (name: string) => {
      const t = dom.tabs.find((x) => x.text.startsWith(name));
      if (!t) return 'none';
      if (/^\d+$/.test(t.count)) return t.selected && dom.notes.some((n) => n.startsWith('Stale: the last refresh failed')) || t.title.startsWith('Stale') ? 'stale' : 'ok';
      if (t.count === '…') return 'loading';
      if (t.count === '!') return 'failed';
      return `unknown: ${t.count}`;
    };
    sess = status('Session edits');
    tree = status('Working tree');
  } else if (dom.label === null) {
    // No chip on a thread whose header is up: a failed read, both
    // reads fail together ("A failed read is no value").
    sess = dom.strip ? 'failed' : 'no header';
    tree = sess;
  } else {
    const m = /^(?:No edits\.|Session edits: ([^.]*?)(?:, \d+ added, \d+ removed)?\.) Working tree: (.*?)(, stale)?$/.exec(dom.label);
    const word = (w: string | undefined) => (w === 'Reading…' ? 'loading' : w === 'Unavailable' ? 'failed' : w === undefined || /^(None|\d+ files?)$/.test(w) ? 'ok' : `unknown: ${w}`);
    sess = m ? word(m[1]) : `unknown: ${dom.label}`;
    tree = m ? (m[3] ? 'stale' : word(m[2])) : sess;
  }

  return {
    vp: dom.wide ? 'wide' : 'phone',
    route: dom.page ? 'page' : 'thread',
    pop: dom.pop,
    palette: dom.palette,
    focus: dom.focus,
    back: dom.palette ? c.back : 'body',
    scope: c.scope,
    sess,
    tree,
    pick: dom.picks.join(','),
    diff: dom.diff.length === 0 ? 'none' : dom.diff.join(','),
    edited: c.edited,
  };
}

// --- what every node owes

// Pause the page's clock where it is now. Its time runs on while this is
// asked, so a moment that has passed by the time it lands is asked again.
async function pause(page: Page): Promise<void> {
  for (let i = 0; ; i++) {
    const at = await page.evaluate(() => Date.now() + 100);
    try {
      await page.clock.pauseAt(at);
      return;
    } catch (e) {
      if (i > 20 || !/past/.test(String(e))) throw e;
    }
  }
}

async function invariants(c: Ctx, errors: string[], where: string, route: string, palette: boolean): Promise<void> {
  const { page } = c;
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);

  const carrier = route === 'page' ? page.locator('.chg-page [role="tablist"]') : page.locator('.thread-head h1').first();
  await expect(carrier, `${where}: status not visible`).toBeVisible();

  // Text cut off by its own box in the header or a scope tab: a box that
  // clips (overflow not visible) narrower than what it holds.
  const clipped = await page.evaluate(() => {
    const out: string[] = [];
    const els = document.querySelectorAll<HTMLElement>('.thread-head *, [role="tablist"] *, [role="tablist"]');
    for (const el of els) {
      if (!el.getClientRects().length || !(el.textContent ?? '').trim()) continue;
      const s = getComputedStyle(el);
      if (s.overflowX === 'visible' && s.textOverflow !== 'ellipsis') continue;
      if (el.scrollWidth > el.clientWidth + 1) out.push(`${el.tagName.toLowerCase()}.${[...el.classList].join('.')} "${(el.textContent ?? '').trim().slice(0, 40)}" ${el.scrollWidth}>${el.clientWidth}`);
    }
    return out;
  });
  expect(clipped, `${where}: clipped text`).toEqual([]);

  expect(errors, `${where}: console errors`).toEqual([]);

  // A keyboard focus the eye can find: an outline or a ring of shadow.
  const ring = await page.evaluate(() => {
    const a = document.activeElement as HTMLElement | null;
    // .app is where a route change parks focus, ringless by design (R4-I).
    if (!a || a === document.body || a.classList.contains('app') || !a.matches(':focus-visible')) return null;
    const s = getComputedStyle(a);
    // A text field's caret is its focus mark (the palette's field draws no ring).
    if ((a.tagName === 'INPUT' || a.tagName === 'TEXTAREA') && s.caretColor !== 'transparent') return null;
    const drawn = (s.outlineStyle !== 'none' && parseFloat(s.outlineWidth) > 0) || (s.boxShadow !== 'none' && s.boxShadow !== '');
    return drawn ? null : `${a.tagName.toLowerCase()}.${[...a.classList].join('.')} "${(a.getAttribute('aria-label') ?? a.textContent ?? '').trim().slice(0, 40)}"`;
  });
  expect(ring, `${where}: focused element has no visible focus ring`).toBeNull();

  // The surface on screen: the palette over whatever it covers, else the
  // page, else the thread's header (the chip and its popover live there).
  // The palette's result list is ui_palette's surface and is checked there.
  const surface = palette ? '.pal' : route === 'page' ? '.chg-page, .thread-head' : '.thread-head';
  // A fade still running reads as low contrast: let finite ones end (a
  // paused one never does, hence the cap).
  await Promise.race([sleep(1000), page.evaluate(() => Promise.all(document.getAnimations()
    .filter((a) => a.effect?.getTiming().iterations !== Infinity).map((a) => a.finished.catch(() => {}))))]);
  // axe waits on timers of its own; the page's clock runs for it and is
  // paused again after (every read is held, so a poll that fires is inert).
  await page.clock.resume();
  let violations;
  try {
    let axe = new AxeBuilder({ page }).include(surface);
    if (palette) axe = axe.exclude('#pal-list');
    violations = (await axe.analyze()).violations;
  } finally {
    await pause(page);
  }
  expect(violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(' | ')}`), `${where}: axe on ${surface}`).toEqual([]);
}

// --- the walk

const init = async (page: Page, serve: Serve): Promise<Ctx> => {
  const c: Ctx = {
    page, serve, id: '', dir: fs.mkdtempSync(path.join(serve.work, 'chg-')), edited: false,
    held: [], pass: new Set(), inflight: 0, gen: 0, back: 'body', scope: 'session', turns: [],
  };
  checkout(c);
  const res = await serve.api.post('/api/sessions', { data: { cwd: c.dir, prompt: '' } });
  if (!res.ok()) throw new Error(`create session: ${res.status()} ${await res.text()}`);
  c.id = (await res.json()).session.id;
  const done = (r: { url(): string }) => { if (READ.test(r.url()) && c.inflight > 0) c.inflight--; };
  page.on('requestfinished', done);
  page.on('requestfailed', done);
  await page.route(READ, (route) => onRead(c, route));
  await page.setViewportSize(WIDE);
  await page.clock.install();
  await page.clock.pauseAt(Date.now() + 1000);
  await page.goto(`${serve.url}/#/s/${c.id}`);
  await waitHeld(c, ['edits', 'changes']);
  return c;
};

// The session's first done is recorded and it has left running.
async function waitDone(c: Ctx): Promise<void> {
  const deadline = Date.now() + 30_000;
  for (;;) {
    const res = await c.serve.api.get(`/api/sessions/${c.id}`);
    if (res.ok()) {
      const r = (await res.json()) as { session: { status: string }; entries: { kind: string }[] };
      if (r.session.status !== 'running' && r.entries.some((e) => e.kind === 'done')) return;
    }
    if (Date.now() > deadline) throw new Error(`session ${c.id}: no done recorded`);
    await sleep(50);
  }
}

// llm-control's turns can carry tool calls (control.go); the TS Turn type does not name them.
const callTurn = (calls: { name: string; args: Record<string, unknown> }[]) => ({ mode: 'ok', calls } as unknown as Turn);

const tab = (c: Ctx, name: string) => (c.page.locator('.chg-page').count().then((n) => n > 0)).then((page) =>
  (page ? c.page.locator('.chg-page [role="tab"]') : pop(c).locator('[role="tab"]')).filter({ hasText: name }).first());

async function select(c: Ctx, f: string): Promise<void> {
  if (await onPage(c).count()) {
    await onPage(c).locator('details.chg-card').filter({ has: c.page.locator('.chg-card-path b', { hasText: new RegExp(`^${f}$`) }) })
      .locator('.chg-card-path').click();
  } else {
    await pop(c).locator('.chg-files button').filter({ has: c.page.locator('.mono', { hasText: new RegExp(`^${f}$`) }) }).click();
  }
}

async function poll(c: Ctx): Promise<void> {
  for (let i = 0; i < 5 && !c.held.some((h) => h.kind === 'changes'); i++) {
    await c.page.clock.fastForward(10_000);
    await sleep(30);
  }
  await waitHeld(c, ['changes'], 0);
}

const actions: Record<string, (c: Ctx) => Promise<void>> = {
  OpenPopover: (c) => chipDetails(c).locator('> summary').click(),
  EscPopover: (c) => c.page.keyboard.press('Escape'),
  // A mousedown on the transcript, away from the popover.
  async ClickAway(c) {
    const box = await c.page.locator('.thread').first().boundingBox();
    if (!box) throw new Error('ClickAway: no thread on screen');
    await c.page.mouse.click(box.x + box.width / 2, box.y + box.height - 160);
  },
  // The popover's "Open full view", or the phone chip's link (under Details).
  async OpenPage(c) {
    await remount(c, async () => {
      if (await chipDetails(c).count()) {
        await pop(c).locator('a.chg-full').click();
        return;
      }
      const link = c.page.locator('.thread-head a.rt-link[href$="/changes"]');
      if (!(await link.isVisible())) await c.page.locator('.thread-head details.rt-more > summary').click();
      await link.click();
    });
  },
  // Back on the thread, its chip is mounted anew: its scope is its first
  // one, which a phone's chip does not show.
  // A wide window hides the page's Back (the sidebar is on screen): there
  // the way back is the session's own row, which does the same setSub(null).
  async Back(c) {
    await remount(c, async () => {
      const back = c.page.locator('.thread-head button.back').first();
      if (await back.isVisible()) await back.click();
      else await c.page.locator(`button[role="treeitem"][data-id="${c.id}"]`).first().click();
    });
    c.scope = 'session';
  },
  async EscPage(c) {
    await remount(c, () => c.page.keyboard.press('Escape'));
    c.scope = 'session';
  },
  ToSession: async (c) => (await tab(c, 'Session edits')).click(),
  ToTree: async (c) => (await tab(c, 'Working tree')).click(),
  SelectA: (c) => select(c, 'a'),
  SelectH: (c) => select(c, 'h'),
  async Retry(c) {
    const from = c.held.length;
    const scope = (await onPage(c).count()) ? onPage(c) : pop(c);
    await scope.getByRole('button', { name: 'Retry', exact: true }).click();
    const deadline = Date.now() + 10_000;
    while (!(['edits', 'changes'] as Kind[]).every((k) => c.held.slice(from).some((h) => h.kind === k))) {
      if (Date.now() > deadline) throw new Error('Retry: no new reads');
      await sleep(15);
    }
    await letThrough(c, ['edits', 'changes']);
  },
  async Resize(c) {
    const wide = (c.page.viewportSize()?.width ?? 0) > 720;
    await c.page.setViewportSize(wide ? PHONE : WIDE);
  },
  async OpenPalette(c) {
    // What the palette will give focus back to, read off the DOM now.
    c.back = await c.page.evaluate(() => (document.querySelector('details.rt-jobs:has(a.chg-full)')?.contains(document.activeElement) ? 'chip' : 'body'));
    await c.page.keyboard.press('Control+k');
    await expect(c.page.locator('.pal[role="dialog"]')).toBeVisible();
  },
  EscPalette: (c) => c.page.keyboard.press('Escape'),
  async PaletteReview(c) {
    await remount(c, async () => {
      await c.page.keyboard.type('Review changes');
      await c.page.getByRole('option', { name: /Review changes/ }).first().click();
      await c.page.clock.runFor(20);
    });
  },

  // --- the server
  async Answer(c) {
    await waitHeld(c, ['edits', 'changes']);
    await letThrough(c, ['edits', 'changes']);
  },
  async AnswerFails(c) {
    await waitHeld(c, ['edits', 'changes']);
    failHeld(c, ['edits', 'changes']);
  },
  async DiffAnswer(c) {
    await waitHeld(c, ['diff'], 0);
    await letThrough(c, ['diff']);
  },
  async PollFails(c) {
    await poll(c);
    failHeld(c, ['changes']);
  },
  async Poll(c) {
    await poll(c);
    await letThrough(c, ['edits', 'changes']);
  },
  // A turn of the session's that writes a and b. Its prompt is sent as
  // another client would: the spec's AgentEdit is the server's event,
  // and typing in this page's composer would itself move focus and shut
  // the popover, which the spec does not have it do.
  async AgentEdit(c) {
    const name = `t${String(++turnSeq).padStart(6, '0')}`;
    const q = controlDir(c.serve.home);
    c.turns.push(name + 'a', name + 'b');
    queue(q, name + 'a', callTurn([{ name: 'bash', args: { command: "printf 'agent\\n' > a && printf 'agent\\n' > b" } }]));
    queue(q, name + 'b', { mode: 'ok', text: 'edited a and b' });
    await letThrough(c, ['edits', 'changes'], async () => {
      const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: `edit a and b ${name}` } });
      if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
      await waitDone(c);
      // The last entries reach the page after done; their re-reads too.
      for (let i = 0; i < 10; i++) { await c.page.clock.runFor(16); await sleep(30); }
    });
    c.edited = true;
  },
};

loadPaths(SPEC).forEach((trace, i) => {
  const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
  test(`model: ${SPEC} path ${i}: ${walk}`, async ({ sharedServe, page }) => {
    const errors: string[] = [];
    page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
    page.on('pageerror', (e) => errors.push(String(e)));
    const c = await init(page, sharedServe);
    try {
      for (const [n, step] of trace.entries()) {
        const name = step.action === 'Init' ? 'Init' : step.action.slice(ROLE.length + 1);
        const where = `step ${n} (${name})`;
        if (n > 0) {
          const act = actions[name];
          if (!act) throw new Error(`${SPEC}: no action for ${step.action}`);
          await act(c);
        }
        const want = roleState(ROLE, step.state);
        await expect.poll(async () => {
          await c.page.clock.runFor(50);
          return readUiState(c);
        }, { message: `${where}: state`, timeout: 10_000 }).toEqual(want);
        await invariants(c, errors, where, String(want.route), Boolean(want.palette));
      }
    } finally {
      await page.unrouteAll({ behavior: 'ignoreErrors' });
      for (const h of c.held.splice(0)) h.route.continue().catch(() => {});
      const q = controlDir(sharedServe.home);
      for (const t of c.turns) if (!fs.existsSync(path.join(q, t + '.taken'))) fs.rmSync(path.join(q, t + '.json'), { force: true });
    }
  });
});
