// go/tests/model/specs/stop_interrupt.fizz in the browser (recipe:
// go/tests/model/README.md): Stop pressed in the composer on a prompt the
// child has not taken yet (the server holds it), on a running turn, and on
// a turn that finished before the SIGINT landed, with the child's exit and
// the respawn every Stop costs. The Go twin is
// go/tests/model/mbt/stop_interrupt_test.go; this file drives the same
// steps, except that Prompt and Stop go through the page.
//
// The spec splits what the real system does in one breath into steps, and
// names server internals no page shows. What is read off the DOM, and why
// the rest is not:
//
//   status     the sidebar row's label word
//   pending    a prompt section still on its way (turn-sending)
//   cancelled  the last turn not on its way ends in "Stopped"
//   stopping   the composer's Stop button: "Stopping" | "Retry stop"
//   draft      the composer holds the stopped prompt again
//
//   alive, held, timer, stale, sigint, tail: the supervisor's, never on the
//   page; they are this file's record of the spec (as in the Go adapter,
//   which reads alive off the API).
//   replied: llm-control has no reply that leaves its turn running, so
//   Reply is the record only.
//
// And where the page runs ahead of the spec:
//
//   - Settle is a React effect on `live` (the spec says so): the render
//     that ends the turn already cleared Stopping and put the prompt back.
//     While Settle is enabled in the spec, stopping and draft are the
//     spec's; the step after Settle reads them off the page.
//   - A held Stop goes out the moment the child takes its prompt: Take,
//     ChildSignal and Wrapup in one. Take checks that outcome on the page,
//     and until Wrapup status, cancelled, stopping and draft are the
//     spec's (`ahead`).
//   - A turn the spec says replied, when the real one could not, is
//     stopped with no reply on the page, so its prompt is put back: draft
//     is the spec's then.
//
// The SIGINT of a Stop on a running turn is "on its way" until
// ChildSignal: the page's POST /interrupt is held by a route until then,
// so a Finish in between is a real finish that the SIGINT then reaches
// idle. The child is SIGSTOPped before every prompt and SIGCONTed at Take,
// so "sent and unread" is a state the page can press Stop in.
//
// Not driven, and skipped from that step on (the prefix is checked):
// HoldExpires (after holdLimit, 20 s, the SIGINT and the unread line reach
// the child in the same instant on SIGCONT, and which wins is a race; the
// Go adapter probed it), Resend (reachable only after it), and a Finish of
// a turn the server already cancelled at a held Take.
import { execFileSync } from 'child_process';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

interface Shadow {
  status: string; pending: boolean; alive: boolean; replied: boolean;
  held: boolean; timer: boolean; stale: boolean; sigint: string;
  cancelled: boolean; tail: boolean; stopping: string; draft: string;
}

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  sh: Shadow;
  turn: number;
  name: string;         // the llm-control turn the current prompt takes, '' when none
  pid: number;          // the session's child, 0 when it has none
  frozen: boolean;      // pid is SIGSTOPped
  ahead: boolean;       // the real system ran past the spec's step (see above)
  realReply: boolean;   // the current turn has a recorded assistant reply
  deferStop: boolean;   // the next /interrupt is held until ChildSignal
  stopRoute?: Route;
}

const stopBound = 5_000;

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const WORDS: Record<string, string> = { Idle: 'idle', Running: 'running', Done: 'done', Stopped: 'stopped' };
const settleEnabled = (s: Shadow) => !s.pending && s.status !== 'running' && s.stopping !== '';

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page, sh } = c;
  const label = (await row(c).getAttribute('aria-label')) ?? '';
  const status = WORDS[label.split(', ')[1]] ?? `unknown: ${label}`;
  const pending = (await page.locator('.transcript section.turn-sending').count()) > 0;
  // The last turn on the page that is not still on its way. A running
  // turn may sit in its prompt's own section until the next change event
  // brings the recorded input (the headless child prints none), so this is
  // not only the data-turn sections. A turn with no prompt (the "/think"
  // Prompt sends to respawn a child) is not a turn that can be stopped.
  const last = page.locator('.transcript section.turn:not(.turn-sending):has(.prompt)').last();
  const cancelled = (await last.count()) > 0 && (await last.locator('.turn-stopped').count()) > 0;
  const stopping = (await page.locator('button.composer-stop-retry').count()) > 0 ? 'failed'
    : (await page.locator('button.composer-stop[aria-label="Stopping"]').count()) > 0 ? 'stopping' : '';
  const text = await page.locator('#composer').inputValue();
  const draft = text === '' ? '' : c.name && text === `turn ${c.name}` ? 'restored' : `unknown: ${text}`;
  const pageSettled = settleEnabled(sh) || c.ahead;
  return {
    status: c.ahead ? sh.status : status,
    pending,
    alive: sh.alive,
    replied: sh.replied,
    held: sh.held,
    timer: sh.timer,
    stale: sh.stale,
    sigint: sh.sigint,
    cancelled: c.ahead ? sh.cancelled : cancelled,
    tail: sh.tail,
    stopping: pageSettled ? sh.stopping : stopping,
    draft: pageSettled || (sh.replied && !c.realReply) ? sh.draft : draft,
  };
}

// The session's child among serve's children: a test's serve has one session.
function childPID(c: Ctx): number {
  const deadline = Date.now() + stopBound;
  for (;;) {
    let out = '';
    try { out = execFileSync('pgrep', ['-P', String(c.serve.pid), '-f', '--', '--headless'], { encoding: 'utf8' }); } catch { /* none yet */ }
    const f = out.split(/\s+/).filter(Boolean);
    if (f.length === 1) return Number(f[0]);
    if (Date.now() > deadline) throw new Error(`serve ${c.serve.pid} has ${f.length} headless children, want 1`);
    execFileSync('sleep', ['0.02']);
  }
}

function gone(pid: number): boolean {
  try { process.kill(pid, 0); return false; } catch { return true; }
}

async function waitGone(c: Ctx, what: string): Promise<void> {
  const deadline = Date.now() + stopBound;
  while (c.pid && !gone(c.pid)) {
    if (Date.now() > deadline) throw new Error(`${what}: child ${c.pid} still alive after ${stopBound}ms`);
    await new Promise((r) => setTimeout(r, 20));
  }
  c.pid = 0;
}

// A real outcome the spec reaches over several steps, checked on the page
// at the first of them.
async function waitPage(c: Ctx, what: string, ok: (s: Record<string, unknown>) => boolean): Promise<void> {
  const deadline = Date.now() + stopBound;
  for (;;) {
    await c.page.clock.fastForward(5_000);
    const saved = c.ahead;
    c.ahead = false;
    const s = await readUiState(c);
    c.ahead = saved;
    if (ok(s)) return;
    if (Date.now() > deadline) throw new Error(`${what}: page shows ${JSON.stringify(s)}`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

modelTests<Ctx>({
  spec: 'stop_interrupt',
  role: 'Session#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    // A walk is up to 12 steps, each a poll of up to 10 s in the worst case.
    test.setTimeout(90_000);
    const c: Ctx = {
      page, serve, id: await serve.newSession(), turn: 0, name: '', pid: 0, frozen: false, ahead: false, realReply: false, deferStop: false,
      sh: { status: 'idle', pending: false, alive: true, replied: false, held: false, timer: false, stale: false, sigint: '', cancelled: false, tail: false, stopping: '', draft: '' },
    };
    await page.route('**/api/sessions/*/interrupt', (route) => {
      if (c.deferStop) { c.deferStop = false; c.stopRoute = route; return; }
      return route.continue();
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    return c;
  },

  actions: {
    async Prompt(c) {
      const sh = c.sh;
      if (!sh.alive) {
        // Supervisor.ensure spawns a child on any line; "/think" is not a
        // prompt, so it leaves nothing unread and starts no turn. It goes
        // through the API as another client would: the child has to exist
        // (and be paused) before the prompt reaches it, and a page send
        // would spawn it and hand it the prompt in one go.
        const res = await c.serve.api.post(`/api/sessions/${c.id}/effort`, { data: { effort: 'medium' } });
        if (!res.ok()) throw new Error(`effort: ${res.status()} ${await res.text()}`);
      }
      if (!c.pid) c.pid = childPID(c);
      process.kill(c.pid, 'SIGSTOP');
      c.frozen = true;
      c.name = `t${String(++c.turn).padStart(4, '0')}`;
      c.realReply = false;
      queue(controlDir(c.serve.home), c.name, { mode: 'block', text: `finished ${c.name}` });
      await c.page.locator('#composer').fill(`turn ${c.name}`);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      if (!sh.alive) sh.stale = false;
      Object.assign(sh, { alive: true, pending: true, replied: false, draft: '' });
    },

    async Take(c) {
      const sh = c.sh;
      if (sh.sigint !== '' || !c.frozen) throw new Error(`take: sigint ${sh.sigint} frozen ${c.frozen}: a state this file does not drive`);
      process.kill(c.pid, 'SIGCONT');
      c.frozen = false;
      if (sh.held) {
        // The held Stop goes out as the child takes the prompt: the turn
        // opens and is cancelled, and the child exits.
        await waitPage(c, 'the held stop to cancel the taken prompt', (s) => s.status === 'stopped' && s.cancelled === true && s.pending === false);
        await waitGone(c, 'held stop');
        c.ahead = true;
      } else {
        await waitTaken(controlDir(c.serve.home), c.name);
      }
      Object.assign(sh, { pending: false, status: 'running', cancelled: false, replied: false });
      if (sh.held) Object.assign(sh, { held: false, sigint: 'sent' });
      if (sh.timer) Object.assign(sh, { timer: false, stale: true });
    },

    async Reply(c) { c.sh.replied = true; },

    async Finish(c) {
      if (c.ahead) test.skip(true, 'Finish of a turn the server already cancelled at a held Take');
      release(controlDir(c.serve.home), c.name);
      c.realReply = true;
      c.sh.status = 'done';
    },

    // The composer's Stop button. On a running turn its POST is held until
    // ChildSignal; on an unread prompt it goes straight out and the server
    // holds it.
    async Stop(c) {
      const sh = c.sh;
      c.deferStop = !sh.pending && sh.alive;
      await c.page.getByRole('button', { name: 'Stop', exact: true }).click();
      sh.stopping = 'stopping';
      if (!sh.alive) sh.stopping = 'failed';
      else if (sh.pending) Object.assign(sh, { held: true, timer: true });
      else sh.sigint = 'sent';
    },

    async HoldExpires() { test.skip(true, 'HoldExpires is a race on the real child (see the file comment)'); },
    async Resend() { test.skip(true, 'Resend follows an expired hold only'); },
    async StaleFires(c) { c.sh.stale = false; },

    async ChildSignal(c) {
      const sh = c.sh;
      if (!c.ahead) {
        const deadline = Date.now() + stopBound;
        while (!c.stopRoute) {
          if (Date.now() > deadline) throw new Error('child signal: the page never POSTed /interrupt');
          await new Promise((r) => setTimeout(r, 20));
        }
        const r = c.stopRoute;
        c.stopRoute = undefined;
        await r.continue();
        await waitGone(c, 'SIGINT');
      }
      sh.sigint = '';
      if (sh.status === 'running') Object.assign(sh, { status: 'stopped', cancelled: true, tail: true });
      else Object.assign(sh, { alive: false, stale: false });
    },

    async Wrapup(c) {
      c.ahead = false;
      Object.assign(c.sh, { tail: false, alive: false, stale: false });
    },

    // The page's own effect; the read after it checks it ran.
    async Settle(c) {
      const sh = c.sh;
      sh.stopping = '';
      if (sh.status === 'stopped' && !sh.replied && sh.draft === '') sh.draft = 'restored';
    },
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.frozen && c.pid && !gone(c.pid)) process.kill(c.pid, 'SIGCONT');
    await c.stopRoute?.continue().catch(() => {});
    if (c.name) release(controlDir(c.serve.home), c.name);
  },
});
