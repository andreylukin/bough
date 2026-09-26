// go/tests/model/specs/background_job_survives_restart.fizz in the
// browser (recipe: go/tests/model/README.md). One session, one real
// background bash job, taken through `bough update`/`bough restart`'s
// path (Serve.shutdown SIGKILLs the headless child; the reap goroutine
// writes the orphaned job's forced "finished" before the kill returns)
// while the job is still running.
//
// The flow is about the gap between what actually happened to the job's
// OS process and what a resumed session's page can ever be told: while
// serve is down the page has nothing to poll, so it cannot itself tell
// "the SIGKILL just landed" (child killed, not yet reaped) from "the
// reap already ran" (child gone) — those two spec states are one real
// event on the wire (see the Go adapter's file comment,
// tests/model/mbt/background_job_survives_restart_test.go). This walk
// performs the real thing at each step exactly where the spec's action
// says to — a real Shutdown/Resume, a real OS process the test signals
// directly, a real read of the session's on-disk history — and only
// defers *revealing* child/typed/reachable from "killed"/"started"/true
// to "gone"/"closed"/false until ReapWritesHistory, the same way the Go
// side does. `job` and `running` are never deferred: `running` is a
// live signal (the job's real pid) at every read, and `job` only ever
// changes on the action that spec says changes it.
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  turn: number;
  held: string; // the llm-control turn holding the open request, '' when none
  jobDir: string;
  pid: number; // the job's real OS pid, once started

  child: 'alive' | 'killed' | 'gone' | 'resumed';
  job: 'running' | 'orphaned' | 'exited';
  typed: 'started' | 'closed';
  reachable: boolean;
  delivered: boolean;
  deferredReap: boolean;
}

// The background job: writes its own pid first so the test can check it
// is alive directly (no serve, no session, needed), then waits for an
// "exit" file — a real OS process, not any history file's JSON, is the
// ground truth for it.
function jobScript(dir: string): string {
  return `D=${JSON.stringify(dir)}\necho $$ > "$D/pid"\nwhile [ ! -e "$D/exit" ]; do sleep 0.02; done\nexit 0\n`;
}

function processAlive(pid: number): boolean {
  if (pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function waitFor(what: string, ok: () => boolean | Promise<boolean>, timeoutMs = 20_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await ok()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 30));
  }
}

// Polls the session's on-disk history (real regardless of whether the
// child answering the API is up) for an entry matching ok.
function historyHas(serve: Serve, id: string, ok: (e: { kind?: string; data?: Record<string, unknown> }) => boolean): boolean {
  const p = path.join(serve.home, '.bough', 'history', `${id}.jsonl`);
  if (!fs.existsSync(p)) return false;
  for (const line of fs.readFileSync(p, 'utf8').split('\n')) {
    if (!line.trim()) continue;
    try {
      if (ok(JSON.parse(line))) return true;
    } catch {
      // a line mid-write; the next poll sees it whole.
    }
  }
  return false;
}

async function readPID(c: Ctx): Promise<void> {
  const p = path.join(c.jobDir, 'pid');
  await waitFor('the job pid file', () => {
    if (!fs.existsSync(p)) return false;
    const n = parseInt(fs.readFileSync(p, 'utf8').trim(), 10);
    if (!(n > 0)) return false;
    c.pid = n;
    return true;
  });
}

function nextTurn(c: Ctx): string {
  return `t${String(++c.turn).padStart(4, '0')}`;
}

modelTests<Ctx>({
  spec: 'background_job_survives_restart',
  role: 'Session#0',
  config: CONTROL_CONFIG,

  // The job's first call and the hold that follows come from llm-control,
  // driven through the page's own composer, the way a person starting a
  // background job would.
  async init(page, serve) {
    test.setTimeout(60_000);
    const id = await serve.newSession();
    const jobDir = path.join(serve.home, 'bjsr-job');
    fs.mkdirSync(jobDir, { recursive: true });
    const c: Ctx = {
      page, serve, id, turn: 0, held: '', jobDir, pid: 0,
      child: 'alive', job: 'running', typed: 'started', reachable: true, delivered: false, deferredReap: false,
    };
    await page.goto(`${serve.url}/#/s/${id}`);
    const dir = controlDir(serve.home);
    const call = nextTurn(c);
    queue(dir, call, { mode: 'call', tool: 'bash', args: { command: jobScript(jobDir), background: true, timeout: '300s' } });
    const hold = nextTurn(c);
    queue(dir, hold, { mode: 'block', text: 'holding' });
    await c.page.locator('#composer').fill('start a background job');
    await c.page.getByRole('button', { name: 'Send', exact: true }).click();
    await waitTaken(dir, call);
    await waitTaken(dir, hold);
    c.held = hold;
    await readPID(c);
    await waitFor("the job's started entry", () => historyHas(serve, id, (e) => e.kind === 'job' && e.data?.event === 'started'));
    return c;
  },

  actions: {
    // The command exits on its own while the child is still up: Jobs
    // delivers the notice in-process, live.
    async JobFinishesNaturally(c) {
      fs.writeFileSync(path.join(c.jobDir, 'exit'), '');
      await waitFor("the job's natural finish", () => historyHas(c.serve, c.id, (e) => e.kind === 'job' && e.data?.event === 'finished' && e.data?.stopped !== true));
      await waitFor('the job process to exit', () => !processAlive(c.pid));
      c.job = 'exited';
      c.typed = 'closed';
      c.reachable = false;
      c.delivered = true;
    },

    // `bough update`/`bough restart`: the real SIGTERM path (Serve.shutdown
    // -> Supervisor.Close) SIGKILLs the headless child and, before it
    // returns, the reap goroutine has already forced the orphan job's
    // "finished, stopped:true" entry onto disk. Confirmed for real here;
    // held back from the spec's view (see the file comment) until
        // ReapWritesHistory.
    async RestartKillsChild(c) {
      const wasRunning = c.job === 'running';
      await c.serve.shutdown();
      if (wasRunning) {
        await waitFor("the reaper's forced finish", () => historyHas(c.serve, c.id, (e) => e.kind === 'job' && e.data?.event === 'finished' && e.data?.stopped === true));
        c.job = 'orphaned';
      }
      c.child = 'killed';
      c.deferredReap = true;
    },

    // Nothing left to do to the real system — the reap already ran,
    // inside RestartKillsChild's shutdown — only to reveal it now that
    // the spec is ready to see it.
    async ReapWritesHistory(c) {
      c.child = 'gone';
      c.typed = 'closed';
      c.reachable = false;
      c.deferredReap = false;
    },

    // serve comes back up, and the session's next input is what actually
    // spawns its fresh, empty-registry child — the page's own composer.
    async NewChildStarts(c) {
      await c.serve.resume();
      const dir = controlDir(c.serve.home);
      const name = nextTurn(c);
      queue(dir, name, { mode: 'ok', text: 'hi again' });
      await c.page.locator('#composer').fill('are you there');
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      await waitTaken(dir, name);
      await waitFor('the resumed child to answer', async () => {
        const res = await c.serve.api.get(`/api/sessions/${c.id}`);
        return res.ok() && (await res.json()).session.status === 'done';
      });
      c.child = 'resumed';
      c.held = '';
    },

    // The orphaned process is not immortal: nothing in bough is watching
    // it any more, but it ends on its own, sometime.
    async OrphanProcessExits(c) {
      fs.writeFileSync(path.join(c.jobDir, 'exit'), '');
      await waitFor('the orphaned job to exit', () => !processAlive(c.pid));
    },
  },

  // The offline stretch between RestartKillsChild and NewChildStarts
  // fails every request the page happens to have in flight.
  allowConsole: /^Failed to load resource: net::ERR_CONNECTION_REFUSED$/,

  read: async (c) => ({
    child: c.child,
    job: c.job,
    typed: c.typed,
    running: processAlive(c.pid),
    reachable: c.reachable,
    delivered: c.delivered,
  }),
  // The sidebar row: the one surface that is on the page whether or not
  // serve itself is reachable right now.
  status: (c) => c.page.locator(`button.row[data-id="${c.id}"]`).first(),
  sessions: (c) => [c.id],
  async cleanup(c) {
    try {
      fs.writeFileSync(path.join(c.jobDir, 'exit'), '');
    } catch {
      // already gone
    }
    if (c.held) {
      try {
        release(controlDir(c.serve.home), c.held);
      } catch {
        // serve may already be down
      }
    }
  },
});
