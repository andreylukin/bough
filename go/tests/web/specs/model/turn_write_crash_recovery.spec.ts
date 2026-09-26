// go/tests/model/specs/turn_write_crash_recovery.fizz in the browser
// (recipe: go/tests/model/README.md). One session, one real crash mid
// write of its history.jsonl (a real SIGKILL of the child whose cwd is
// the walk's, then the fragment a torn write leaves appended to the real
// file), and a real turn through the composer to prove serve's own
// resume path (ensure -> spawn -> the history plugin's OpenExisting)
// recovers it. The Go twin is
// tests/model/mbt/turn_write_crash_recovery_test.go.
//
// torn and loaded are the flow's own record, as provider_failure_retry.spec.ts
// keeps req/attempt/paid: nothing on the page ever shows a history file's
// bytes or whether its Store is open in the crashed child's place.
// Reopen has no reachable UI or API of its own — the Go adapter calls
// history.OpenExisting on the file directly, a check the Go MBT test
// above already owns — so here it only flips the record; the real
// recovery this flow proves is the Append right after it: a real prompt
// through the composer, on the session serve just watched crash, that
// still completes.
import type { Page } from '@playwright/test';
import * as fs from 'fs';
import * as path from 'path';
import { execFileSync } from 'child_process';
import { CONTROL_CONFIG, controlDir, queue, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string; // llm-control's queue
  n: number;   // turn names are unique across the walk
  torn: boolean;
  loaded: boolean;
}

// Same fragment as the Go adapter's twcFragment (mbt/turn_write_crash_recovery_test.go):
// a write cut mid-line, no closing brace, no trailing newline.
const FRAGMENT = '{"kind":"twc-crash-fragment","data":{"text":"cut';

function histPath(c: Ctx): string {
  return path.join(c.serve.home, '.bough', 'history', `${c.id}.jsonl`);
}

async function waitFor(what: string, ok: () => boolean | Promise<boolean>, timeoutMs = 20_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (await ok()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 30));
  }
}

// Whether the walk's own bough child (the session's cwd is serve.work,
// one local session per test) is still alive: the same lsof-by-cwd
// lookup native_call_adoption.spec.ts uses for a real crash.
function childPid(cwd: string): number {
  let out = '';
  try {
    out = execFileSync('lsof', ['-a', '-d', 'cwd', '-c', 'bough', '-Fpn'], { encoding: 'utf8' });
  } catch {
    return 0; // lsof exits non-zero when nothing matches
  }
  let pid = 0;
  for (const l of out.split('\n')) {
    if (l.startsWith('p')) pid = Number(l.slice(1));
    else if (l.startsWith('n') && l.slice(1) === cwd && pid > 0) return pid;
  }
  return 0;
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const entries = await c.page.locator('.transcript section.turn .turn-foot').count();
  return { entries, torn: c.torn, loaded: c.loaded };
}

modelTests<Ctx>({
  spec: 'turn_write_crash_recovery',
  role: 'HistoryFile#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const id = await serve.newSession();
    const c: Ctx = { page, serve, id, dir: controlDir(serve.home), n: 0, torn: false, loaded: true };
    await page.goto(`${serve.url}/#/s/${id}`);
    return c;
  },

  actions: {
    // One whole turn through the composer, on whatever child answers it
    // (a fresh one, if the last step crashed the old one).
    async Append(c) {
      const name = `t${String(++c.n).padStart(5, '0')}`;
      queue(c.dir, name, { mode: 'ok', text: `finished ${name}` });
      await c.page.locator('#composer').fill(`turn ${name}`);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      await waitTaken(c.dir, name);
      await waitFor('the turn to finish', async () => {
        const res = await c.serve.api.get(`/api/sessions/${c.id}`);
        return res.ok() && (await res.json()).session.status !== 'running';
      });
      c.torn = false;
    },

    // A real crash: SIGKILL the child working in the session's cwd (a
    // real interrupted write, so nothing pending reaches the file),
    // then leave the file ending in a fragment with no trailing
    // newline, exactly what a write cut mid-append leaves.
    async CrashMidAppend(c) {
      const cwd = fs.realpathSync(c.serve.work);
      const pid = childPid(cwd);
      if (pid === 0) throw new Error(`no bough child in ${cwd} to kill`);
      process.kill(pid, 'SIGKILL');
      await waitFor('the crashed child to exit', () => childPid(cwd) === 0);
      fs.appendFileSync(histPath(c), FRAGMENT);
      c.torn = true;
      c.loaded = false;
    },

    // No UI or API reaches history.OpenExisting directly (the Go MBT
    // test above calls it on the file itself); the real recovery this
    // flow can exercise is the next real Append, which resumes the
    // session (serve's ensure -> spawn -> the history plugin's
    // OpenExisting) the ordinary way. This step only flips the record.
    async Reopen(c) {
      c.torn = false;
      c.loaded = true;
    },
  },

  read: readUiState,
  status: (c) => c.page.locator(`button.row[data-id="${c.id}"]`).first(),
  sessions: (c) => [c.id],
});
