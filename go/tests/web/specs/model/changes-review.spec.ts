// go/tests/model/specs/changes_review.fizz in the browser: the Changes
// page (#/s/<id>/changes) for three sessions, A (a checkout with
// checkpoints whose agent edits "a" through the shell and whose "h" a
// person edits by hand), N (a checkout whose one turn has no
// checkpoint) and R (no repository). Every generated path is walked
// through the page against a real serve whose model is llm-control; at
// every node readUiState must equal the spec's role state (recipe:
// go/tests/model/README.md).
//
// The spec's reads (GET edits, GET edits?turn=, GET changes) answer in
// steps of their own; on their own they are over in milliseconds, and
// the tree is re-read every 10 page-seconds. So the walk holds them in
// the browser while a step needs them held:
//
//   - Open holds the opening reads until Answer lets them go to the
//     server, or AnswerFails fails them.
//   - HandEdit and PollFails hold the poll, so the page keeps the tree
//     it has until Poll (or Retry) lets one through, or PollFails fails
//     it.
//   - Answer, Poll, Retry, AgentEdit and Close let everything through.
//
// Diffs (GET diff) are never held: the spec has no step for them.
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, type Turn } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

// A path is up to seven steps, some of them a real turn.
test.describe.configure({ timeout: 90_000 });

type Sid = 'A' | 'N' | 'R';

interface Ctx {
  page: Page;
  serve: Serve;
  ids: Record<Sid, string>; // '' until made
  aDir: string;
  sid: Sid;             // the session the thread shows: SelectNext's cycle
  n: number;            // turn names, unique per serve
  edited: boolean;      // A's world: its agent edited "a"
  hand: number;         // A's world: hand edits appended to "h"
  hold: boolean;        // reads are held, not let through
  held: Route[];        // reads the walk holds
  inflight: number;     // reads let through, not answered yet
  open: boolean;        // the last read found the page open
}

const READ = /\/api\/sessions\/[^/]+\/(edits|changes)(\?|$)/;

function git(c: Ctx, dir: string, ...args: string[]): void {
  execFileSync('git', ['-C', dir, '-c', 'user.name=model', '-c', 'user.email=model@test', '-c', 'commit.gpgsign=false', ...args], {
    env: { ...process.env, HOME: c.serve.home, GIT_CONFIG_NOSYSTEM: '1' },
    stdio: 'pipe',
  });
}

// A repository with f committed and then changed: dirty from the start.
function checkout(c: Ctx, name: string, f: string): string {
  const dir = path.join(c.serve.work, name);
  fs.mkdirSync(dir, { recursive: true });
  git(c, dir, 'init', '-q', '-b', 'main');
  fs.writeFileSync(path.join(dir, f), 'one\n');
  git(c, dir, 'add', f);
  git(c, dir, 'commit', '-q', '-m', 'base');
  fs.writeFileSync(path.join(dir, f), 'one\ntwo\n');
  return dir;
}

async function create(c: Ctx, cwd: string): Promise<string> {
  const res = await c.serve.api.post('/api/sessions', { data: { cwd, prompt: '' } });
  if (!res.ok()) throw new Error(`create session in ${cwd}: ${res.status()} ${await res.text()}`);
  return (await res.json()).session.id;
}

async function prompt(c: Ctx, id: string, text: string): Promise<void> {
  const res = await c.serve.api.post(`/api/sessions/${id}/prompt`, { data: { text } });
  if (!res.ok()) throw new Error(`prompt ${id}: ${res.status()} ${await res.text()}`);
}

// The session's nth done is recorded and its row has left running.
async function waitDone(c: Ctx, id: string, n: number): Promise<void> {
  const deadline = Date.now() + 30_000;
  for (;;) {
    const res = await c.serve.api.get(`/api/sessions/${id}`);
    if (res.ok()) {
      const r = (await res.json()) as { session: { status: string }; entries: { kind: string }[] };
      if (r.session.status !== 'running' && r.entries.filter((e) => e.kind === 'done').length >= n) return;
    }
    if (Date.now() > deadline) throw new Error(`session ${id}: done #${n} not recorded`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

// llm-control's turns can carry tool calls (control.go); the TS Turn
// type does not name them.
const callTurn = (calls: { name: string; args: Record<string, unknown> }[]) => ({ mode: 'ok', calls } as unknown as Turn);

// N: its one turn writes "n" with the write tool while .git/objects
// cannot be written, so the checkpoint before it fails and only the
// write tool's tally records "n" (patch false: "No diff").
async function makeN(c: Ctx): Promise<string> {
  const dir = checkout(c, 'nocp', 'n');
  const objects = path.join(dir, '.git', 'objects');
  const chmodTree = (mode: number) => {
    const walk = (p: string) => {
      fs.chmodSync(p, mode);
      for (const e of fs.readdirSync(p, { withFileTypes: true })) if (e.isDirectory()) walk(path.join(p, e.name));
    };
    walk(objects);
  };
  chmodTree(0o555);
  try {
    const id = await create(c, dir);
    const q = controlDir(c.serve.home);
    queue(q, '000n1', callTurn([{ name: 'write', args: { path: 'n', content: 'one\ntwo\nthree\n' } }]));
    queue(q, '000n2', { mode: 'ok', text: 'wrote n' });
    await prompt(c, id, 'write n');
    await waitDone(c, id, 1);
    return id;
  } finally {
    chmodTree(0o755);
  }
}

async function ensure(c: Ctx, sid: Sid): Promise<string> {
  if (!c.ids[sid]) c.ids[sid] = sid === 'N' ? await makeN(c) : await create(c, fs.mkdtempSync(path.join(c.serve.work, 'plain-')));
  return c.ids[sid];
}

// --- the reads the walk holds

function onRead(c: Ctx, route: Route): void {
  if (c.hold) { c.held.push(route); return; }
  pass(c, route);
}

function pass(c: Ctx, route: Route): void {
  c.inflight++;
  route.continue().catch(() => {});
}

// Let everything through from now on, and what is held now.
function letGo(c: Ctx): void {
  c.hold = false;
  for (const r of c.held.splice(0)) pass(c, r);
}

// Hold from now on, once what was let through has been answered: a read
// in flight that reached the server after a hand edit would show it.
async function holdReads(c: Ctx): Promise<void> {
  c.hold = true;
  const deadline = Date.now() + 10_000;
  while (c.inflight > 0) {
    if (Date.now() > deadline) throw new Error(`${c.inflight} reads still in flight`);
    await new Promise((r) => setTimeout(r, 20));
  }
}

// A failed read as the page sees it. Not a 5xx: Chromium logs every
// one as a console error, which the walk counts against the page. A
// body that is not JSON fails the same .catch in useChanges.
function fail(route: Route): void {
  route.fulfill({ status: 200, contentType: 'application/json', body: 'not json' }).catch(() => {});
}

const heldTree = (c: Ctx) => c.held.filter((r) => /\/changes(\?|$)/.test(r.request().url()));
const heldEdits = (c: Ctx, turn: boolean) => c.held.some((r) => {
  const u = new URL(r.request().url());
  return /\/edits$/.test(u.pathname) && u.searchParams.has('turn') === turn;
});

// --- the page, as a person sees it

const row = (c: Ctx, sid: Sid) => c.page.locator(`button.row[data-id="${c.ids[sid]}"]`).first();
const tablist = (c: Ctx) => c.page.locator('.chg-page [role="tablist"][aria-label="Changes scope"]');
const TAB: Record<string, string> = { 'This turn': 'turn', 'Session edits': 'session', 'Working tree': 'tree' };

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await c.page.evaluate(() => {
    const tabs = [...document.querySelectorAll('.chg-page [role="tablist"] [role="tab"]')].map((t) => {
      const count = t.querySelector('.seg-count');
      return {
        name: (t.firstChild?.textContent ?? '').trim(),
        selected: t.getAttribute('aria-selected') === 'true',
        count: (count?.textContent ?? '').trim(),
        title: count?.getAttribute('title') ?? '',
      };
    });
    const cards = [...document.querySelectorAll('.chg-page details.chg-card')].map((d) => ({
      name: d.querySelector('.chg-card-path b')?.textContent ?? '',
      open: (d as HTMLDetailsElement).open,
      diff: d.querySelectorAll('pre.rt-diff-body .dl').length > 0,
      add: d.querySelector('.chg-card-count .rt-add')?.textContent ?? '',
    }));
    const notes = [...document.querySelectorAll('.chg-page .rt-label')].map((e) => e.textContent ?? '');
    return { hash: window.location.hash, tabs, cards, notes };
  });

  const m = /^#\/s\/([^/?]+)(\/changes)?(\?.*)?$/.exec(dom.hash);
  const id = m?.[1] ?? '';
  const sid = (Object.keys(c.ids) as Sid[]).find((k) => c.ids[k] === id) ?? `unknown: ${dom.hash}`;
  const open = !!m?.[2] && dom.tabs.length > 0;
  c.open = open;
  const base = {
    open, sid, turn: false, scope: 'session', edits: 'none', tree: 'none', eseen: false, tagent: false, behind: false,
    ca: false, ch: false, cn: false, diff: [] as string[], edited: c.edited,
  };
  if (!open) return base;

  const tab = (s: string) => dom.tabs.find((t) => TAB[t.name] === s);
  const scope = TAB[dom.tabs.find((t) => t.selected)?.name ?? ''] ?? 'none';
  // The shown scope says its state in words; another tab only in its count.
  const status = (s: string) => {
    const t = tab(s);
    if (!t) return 'none';
    if (s === scope) {
      if (dom.notes.some((n) => n.startsWith('Couldn’t read the changes'))) return 'failed';
      if (dom.notes.some((n) => n.startsWith('Stale: the last refresh failed'))) return 'stale';
    } else if (t.title.startsWith('Stale')) return 'stale';
    if (t.count === '…') return 'loading';
    if (/^\d+$/.test(t.count)) return 'ok';
    if (t.title.startsWith('Couldn’t read')) return 'failed';
    return `unknown: ${t.count}`;
  };
  const edits = status('session');
  const turnTab = tab('turn');
  const tree = status('tree');
  const has = (f: string) => dom.cards.some((k) => k.name === f);
  const count = (s: string) => Number(tab(s)?.count);
  const card = (f: string) => dom.cards.find((k) => k.name === f)?.open ?? false;
  // Behind: the h the tree shows is not the h on disk (committed "one",
  // then "two", then one line per hand edit).
  const h = dom.cards.find((k) => k.name === 'h');
  return {
    ...base,
    turn: !!turnTab,
    scope,
    // One state for the session's and the turn's read: they fire and fail together.
    edits: turnTab && status('turn') !== edits ? `session ${edits}, turn ${status('turn')}` : edits,
    tree,
    eseen: sid === 'A' && (edits === 'ok' ? (scope === 'session' ? has('a') : count('session') > 0) : false),
    tagent: sid === 'A' && (tree === 'ok' || tree === 'stale') && (scope === 'tree' ? has('a') : count('tree') === 2),
    behind: sid === 'A' && scope === 'tree' && !!h && h.add !== `+${1 + c.hand}`,
    ca: card('a'), ch: card('h'), cn: card('n'),
    diff: dom.cards.filter((k) => k.open && k.diff).map((k) => k.name),
  };
}

// Waits until the reads the walk holds satisfy ok.
async function waitHeld(c: Ctx, ok: () => boolean): Promise<void> {
  const deadline = Date.now() + 10_000;
  while (!ok()) {
    if (Date.now() > deadline) throw new Error(`held reads not there: ${c.held.map((r) => r.request().url()).join(', ') || 'none'}`);
    await new Promise((r) => setTimeout(r, 20));
  }
}

// Moves the page's clock one interval, which fires exactly one tree
// poll, and waits for it to be held. Moving it again while the fetch is
// still on its way fired a second poll that stayed held past the step.
async function nextPoll(c: Ctx): Promise<void> {
  if (!heldTree(c).length) await c.page.clock.fastForward(10_000);
  await waitHeld(c, () => heldTree(c).length > 0);
}

const tabButton = (c: Ctx, name: string) => c.page.locator('.chg-page [role="tab"]', { hasText: name });
const cardSummary = (c: Ctx, f: string) =>
  c.page.locator('.chg-page details.chg-card').filter({ has: c.page.locator('.chg-card-path b', { hasText: new RegExp(`^${f}$`) }) }).locator('.chg-card-path');

modelTests<Ctx>({
  spec: 'changes_review',
  role: 'Page#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const c: Ctx = {
      page, serve, ids: { A: '', N: '', R: '' }, aDir: '', sid: 'A', n: 0, edited: false, hand: 0,
      hold: false, held: [], inflight: 0, open: false,
    };
    const done = (r: { url(): string }) => { if (READ.test(r.url()) && c.inflight > 0) c.inflight--; };
    page.on('requestfinished', done);
    page.on('requestfailed', done);
    await page.route(READ, (route) => onRead(c, route));
    c.aDir = checkout(c, 'a', 'h');
    c.ids.A = await create(c, c.aDir);
    await page.goto(`${serve.url}/#/s/${c.ids.A}`);
    return c;
  },

  actions: {
    // The sidebar row of the next session in A → N → R → A.
    async SelectNext(c) {
      const next: Sid = c.sid === 'A' ? 'N' : c.sid === 'N' ? 'R' : 'A';
      await ensure(c, next);
      // A session made just now is in the list at its next poll.
      await c.page.clock.fastForward(5_000);
      await row(c, next).click();
      c.sid = next;
    },

    // The header's Changes chip, then "Open full view".
    async Open(c) {
      await holdReads(c);
      await c.page.locator('details.rt-jobs:has(a.chg-full) > summary').click();
      await c.page.getByRole('link', { name: 'Open full view' }).click();
    },

    // The turn footer's "1 file" link.
    async OpenTurn(c) {
      await holdReads(c);
      await c.page.locator('.thread button.turn-files').first().click();
    },

    async Close(c) {
      letGo(c);
      await c.page.locator('body').click({ position: { x: 1, y: 1 }, force: true }).catch(() => {});
      await c.page.keyboard.press('Escape');
    },

    ToTurn: (c) => tabButton(c, 'This turn').click(),
    ToSession: (c) => tabButton(c, 'Session edits').click(),
    ToTree: (c) => tabButton(c, 'Working tree').click(),
    ExpandA: (c) => cardSummary(c, 'a').click(),
    // Click, then let go: a poll held since the failure (the state
    // checks move the page clock) would otherwise answer first and take
    // the Retry button away before the click lands.
    async Retry(c) {
      await c.page.locator('.chg-page').getByRole('button', { name: 'Retry' }).click();
      letGo(c);
    },

    async Answer(c) { letGo(c); },

    async AnswerFails(c) {
      // The opening reads, the tree's included, have all gone out: useChanges
      // sends the session's edits, the turn's (on a turn link), then the tree.
      const turn = (await c.page.evaluate(() => location.hash)).includes('turn=');
      await waitHeld(c, () => heldTree(c).length > 0 && heldEdits(c, false) && (!turn || heldEdits(c, true)));
      for (const r of c.held.splice(0)) fail(r);
    },

    async Poll(c) {
      const had = heldTree(c).length > 0;
      letGo(c);
      if (!had) await c.page.clock.fastForward(10_000);
    },

    async PollFails(c) {
      await holdReads(c);
      await nextPoll(c);
      for (const r of heldTree(c)) { c.held.splice(c.held.indexOf(r), 1); fail(r); }
    },

    // A turn of A's whose bash call writes "a": the composer when A's
    // thread is on screen, else the same POST from another client (the
    // Changes page and another session's thread have no composer for A).
    async AgentEdit(c) {
      letGo(c);
      const name = `t${String(++c.n).padStart(4, '0')}`;
      const q = controlDir(c.serve.home);
      queue(q, name + 'a', callTurn([{ name: 'bash', args: { command: "printf 'agent\\n' > a" } }]));
      queue(q, name + 'b', { mode: 'ok', text: 'edited a' });
      if (c.sid === 'A' && !c.open) {
        await c.page.locator('#composer').fill(`edit a ${name}`);
        await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      } else {
        await prompt(c, c.ids.A, `edit a ${name}`);
      }
      await waitDone(c, c.ids.A, 1);
      c.edited = true;
    },

    // An edit to "h" made outside bough: no entry, no tick.
    async HandEdit(c) {
      await holdReads(c);
      fs.appendFileSync(path.join(c.aDir, 'h'), 'by hand\n');
      c.hand++;
    },
  },

  read: readUiState,
  status: (c) => (c.open ? tablist(c) : row(c, c.sid)),
  sessions: (c) => [c.ids.A],
  async cleanup(c) {
    letGo(c);
  },
});
