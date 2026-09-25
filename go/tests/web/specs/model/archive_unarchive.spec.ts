// go/tests/model/specs/archive_unarchive.fizz in the browser: one open
// session that may start one background agent, archived and unarchived
// through the thread (Session settings > Archive…, the confirm or the
// choice dialog, the composer note's Unarchive). The Go twin is
// go/tests/model/mbt/archive_unarchive_test.go; the background agent is
// set up here exactly as there, so the two walks see the same server.
import type { APIResponse, Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;    // llm-control's queue
  id: string;     // the session the spec is about
  filler: string; // parent of the blockers; kept down, so never woken
  agent: string;  // the agent's id, '' before Spawn
  held: string;   // the agent's turn in flight, '' when none
  block: string;  // the blocker's held turn, '' when none
  turn: number;
}

interface Row { id: string; status: string; live: boolean; archived: boolean }
interface Entry { kind: string; text: string }

const WAIT_MS = 20_000;

async function ok<T>(res: APIResponse, what: string): Promise<T> {
  if (!res.ok()) throw new Error(`${what}: ${res.status()} ${await res.text()}`);
  return (await res.json()) as T;
}

const post = (c: Ctx, path: string, data?: unknown) => c.serve.api.post(path, data === undefined ? {} : { data });
const getSession = async (c: Ctx, id: string) =>
  ok<{ session: Row; entries: Entry[] }>(await c.serve.api.get(`/api/sessions/${id}`), `get ${id}`);

// Waiting on the server is only how an action knows its step has landed
// before the next one; every state the walk checks is read off the page.
async function until<T>(what: string, f: () => Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + WAIT_MS;
  let last: unknown;
  for (;;) {
    try {
      const v = await f();
      if (v !== undefined) return v;
    } catch (e) { last = e; }
    if (Date.now() > deadline) throw new Error(`waiting for ${what}${last ? `: ${last}` : ''}`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

const waitRow = (c: Ctx, id: string, what: string, pred: (r: Row) => boolean) =>
  until(what, async () => { const { session } = await getSession(c, id); return pred(session) ? session : undefined; });

const next = (c: Ctx) => `t${String(++c.turn).padStart(5, '0')}`;

// serve's reportText header: [agent <title> · <id> <word>].
const REPORT = /\[agent [^\]]* · (\S+) (finished|stopped|failed)\]/;

// tools.spawn({background}) is this request (plugins/workers/background.go);
// a turn scripting the tool call would send the same one.
async function spawn(c: Ctx, parent: string, prompt: string, maxRunning: number): Promise<{ id: string; queued: boolean }> {
  const r = await ok<{ session: Row; queued?: boolean }>(
    await post(c, '/api/sessions', { prompt, spawnedBy: parent, maxRunning }), 'spawn');
  return { id: r.session.id, queued: Boolean(r.queued) };
}

// Ends the filler's child and blockers and leaves it unarchived, as the
// Go adapter does: a blocker's report then lands in its file instead of
// waking a turn that would take the next queued one.
async function downFiller(c: Ctx): Promise<void> {
  await ok(await post(c, `/api/sessions/${c.filler}/archive`, { stopChildren: true }), 'archive filler');
  await ok(await post(c, `/api/sessions/${c.filler}/unarchive`), 'unarchive filler');
}

async function releaseBlocker(c: Ctx): Promise<void> {
  release(c.dir, c.block);
  c.block = '';
  const { children } = await ok<{ children: Row[] }>(await c.serve.api.get(`/api/sessions/${c.filler}/children`), 'filler children');
  for (const k of children) await waitRow(c, k.id, 'the blocker to finish', (r) => r.status !== 'running');
}

async function waitAgentRunning(c: Ctx): Promise<void> {
  await waitTaken(c.dir, c.held, WAIT_MS);
  await waitRow(c, c.agent, 'the agent running', (r) => r.status === 'running');
}

const dialog = (c: Ctx) => c.page.locator('[role="dialog"][aria-modal="true"]');

// Archive's dialog answered. The step is the whole POST: serve saves the
// flag before it ends the children, so the row saying archived is not
// yet the end of it, and freeing the blocker's slot then (StopAndArchive)
// raced the drop of the queued agent.
async function answer(c: Ctx, button: string): Promise<void> {
  const done = c.page.waitForResponse((r) => r.url().endsWith(`/api/sessions/${c.id}/archive`) && r.request().method() === 'POST');
  await dialog(c).getByRole('button', { name: button, exact: true }).click();
  const res = await done;
  if (!res.ok()) throw new Error(`archive: ${res.status()} ${await res.text()}`);
}

// The Work button's label (workSummaryText): "Work" alone with nothing,
// else "Work, 1 running" and so on; the spec's words for the one agent.
// A finished agent whose report landed as a notice (its parent not live)
// also counts ", 1 new result" until this viewer opens it: that is review
// state, per viewer and relative to when the page loaded, not the agent's.
const AGENT: Record<string, string> = { 'Work': 'none', 'Work, 1 running': 'running', 'Work, 1 queued': 'queued', 'Work, 1 finished': 'done', 'Work, 1 stopped': 'stopped' };
const agentOf = (label: string) => AGENT[label.replace(/, 1 new result$/, '')] ?? `unknown: ${label}`;

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const work = p.locator('button.work-summary');
  const workLabel = (await work.count()) ? (await work.getAttribute('aria-label')) ?? '' : 'Work';
  const modal = dialog(c);
  const modalText = (await modal.count()) ? (await modal.innerText()) : '';
  // live has no surface on the page: nothing it draws says whether the
  // session's child is up. It is read off serve, as another client would.
  const { session } = await getSession(c, c.id);
  return {
    history: !(await p.locator('.thread-empty').isVisible()),
    live: session.live,
    archived: await p.locator('.archived-note').isVisible(),
    // In the main list: a row for it outside the Archived section.
    listed: (await p.locator(`button.row[data-id="${c.id}"]`).count()) >
      (await p.locator(`#sec-archived button.row[data-id="${c.id}"]`).count()),
    dialog: /Archive this session\?/.test(modalText) ? 'confirm' : /Stop its .* too\?/.test(modalText) ? 'choice' : modalText ? `other: ${modalText}` : 'none',
    agent: agentOf(workLabel),
  };
}

modelTests<Ctx>({
  spec: 'archive_unarchive',
  role: 'Session#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const c: Ctx = { page, serve, dir: controlDir(serve.home), id: '', filler: '', agent: '', held: '', block: '', turn: 0 };
    const name = next(c);
    queue(c.dir, name, { mode: 'ok', text: 'filler' });
    c.filler = await serve.newSession(`filler ${name}`);
    await waitRow(c, c.filler, "the filler's turn", (r) => r.status === 'done');
    await downFiller(c);
    // Create leases a child at once; the spec starts from a session with
    // none (as after a serve restart), which archive + unarchive leaves.
    c.id = await serve.newSession();
    // Every verb 404s until the child has written its history file, which
    // can be after GET already answers.
    await until("the new session's file", async () => {
      const res = await post(c, `/api/sessions/${c.id}/archive`);
      if (res.status() === 404) return undefined;
      return ok(res, 'archive');
    });
    await ok(await post(c, `/api/sessions/${c.id}/unarchive`), 'unarchive');
    await page.goto(`${serve.url}/#/s/${c.id}`);
    return c;
  },

  actions: {
    // The composer; the step ends once the turn is answered and the
    // session idle with its child up.
    async Prompt(c) {
      const name = next(c);
      const text = `prompt ${name}`;
      queue(c.dir, name, { mode: 'ok', text: `answered ${name}` });
      await c.page.locator('#composer').fill(text);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      await until(`${text} answered`, async () => {
        const { session, entries } = await getSession(c, c.id);
        const at = entries.findIndex((e) => e.kind === 'input' && e.text.includes(text));
        const closed = at >= 0 && entries.slice(at + 1).some((e) => e.kind === 'done' || e.kind === 'cancelled');
        return closed && session.status !== 'running' && session.live ? true : undefined;
      });
    },

    // No UI starts an agent: the model does, with tools.spawn.
    async Spawn(c) {
      const name = next(c);
      queue(c.dir, name, { mode: 'block', text: `agent ${name} finished` });
      const { id, queued } = await spawn(c, c.id, `agent ${name}`, 16);
      if (queued) throw new Error(`agent ${id} queued with 16 slots`);
      c.agent = id;
      c.held = name;
      await waitAgentRunning(c);
    },

    // The one running slot a maxRunning of 1 allows is held by a blocker
    // of the filler, so this agent waits in the queue.
    async SpawnQueued(c) {
      if (!c.block) {
        const name = next(c);
        queue(c.dir, name, { mode: 'block', text: `blocker ${name}` });
        const { id } = await spawn(c, c.filler, `blocker ${name}`, 16);
        c.block = name;
        await waitTaken(c.dir, name, WAIT_MS);
        await waitRow(c, id, 'the blocker running', (r) => r.status === 'running');
      }
      const { id, queued } = await spawn(c, c.id, 'queued agent', 1);
      if (!queued) throw new Error(`agent ${id} started with the one slot taken`);
      c.agent = id;
    },

    // A slot coming free: the blocker finishes and the queue starts the agent.
    async AgentStart(c) {
      const name = next(c);
      queue(c.dir, name, { mode: 'block', text: `agent ${name} finished` });
      c.held = name;
      await releaseBlocker(c);
      await waitAgentRunning(c);
    },

    // The agent's turn ends; its report reaches the parent (waking a turn
    // there when it is live), and the step waits for that to settle.
    async AgentFinish(c) {
      release(c.dir, c.held);
      c.held = '';
      await waitRow(c, c.agent, 'the agent done', (r) => r.status === 'done');
      await until('the report in the parent', async () => {
        const { session, entries } = await getSession(c, c.id);
        const there = entries.some((e) => REPORT.exec(e.text)?.[1] === c.agent);
        return there && session.status !== 'running' ? true : undefined;
      });
    },

    async Archive(c) {
      await c.page.getByRole('button', { name: 'Session settings' }).click();
      await c.page.locator('.head-pop-item', { hasText: /^Archive…$/ }).click();
      await dialog(c).waitFor();
    },
    Cancel: (c) => dialog(c).getByRole('button', { name: 'Cancel', exact: true }).click(),
    Confirm: (c) => answer(c, 'Archive'),
    async StopAndArchive(c) {
      await answer(c, 'Stop and archive');
      // The stopped agent's held turn died with it; a dropped queued one
      // leaves the blocker holding a slot nothing waits for.
      c.held = '';
      if (c.block) await releaseBlocker(c);
    },
    ArchiveOnly: (c) => answer(c, 'Archive only'),
    async Unarchive(c) {
      await c.page.locator('.archived-note').getByRole('button', { name: 'Unarchive' }).click();
      await waitRow(c, c.id, 'the session unarchived', (r) => !r.archived);
    },
  },

  read: readUiState,
  // The thread's title: on screen whatever the session's state.
  status: (c) => c.page.locator('h1'),
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.block) release(c.dir, c.block);
    if (c.held) release(c.dir, c.held);
  },
});
