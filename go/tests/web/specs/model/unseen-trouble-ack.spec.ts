// go/tests/model/specs/unseen_trouble_ack.fizz in the browser: one web
// session's unseen finish and trouble marks, and every way the control
// room acks them. Every generated path is walked through the page
// against a real serve whose model is llm-control; at every node
// readUiState must equal the spec's role state (recipe:
// go/tests/model/README.md).
//
// What the page shows is read off the page: the sidebar row carries
// status, trouble (the red reason and its Seen button) and unseen ("not
// seen yet"); location.hash and aria-current carry the screen. Three of
// the spec's fields are what serve derives the marks FROM and no page
// shows them: newSinceAck, doneSinceAck and stale. They are read off the
// files serve reads (meta.json's ack, the transcript), as the Go adapter
// does, so the whole state is compared; the marks themselves come only
// from the DOM. testsFailed is on the row except when the row's reason
// is a failure, which outranks it ("Failed"); only then does it come
// from the transcript.
//
// The steps that are not a click:
//   - AutoAck is the page's own ack of a shown unseen finish. The walk
//     holds that POST in the browser from the moment the page sends it
//     until the AutoAck step lets it go, so "shown and briefly unseen"
//     is a node that can be looked at. An ack the page sends while the
//     session is NOT shown is let through untouched: that is the bug
//     NoAckUntilOnScreen is about, and it shows as a state mismatch.
//   - Expire is seven days passing with nothing written. serve reads the
//     wall clock, so the walk moves the transcript into the past instead:
//     every entry's "at" goes back eight days, the seqs untouched.
//   - Note is an entry after the ack that is no turn: a "!" shell line,
//     which the session records as a command and its output with no
//     input and no done. From the composer when the session is shown,
//     otherwise as another client's POST (a closed session has no
//     composer).
//   - DragDone is a project thread dropped on Done; this session is a
//     local one with no project board, so it is the same POST .../ack
//     from another client.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

type Screen = 'closed' | 'hidden' | 'shown';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;     // the session the spec is about
  decoy: string;  // another session to have on screen: Close opens it
  dir: string;    // llm-control's queue
  turn: number;
  held: string;   // the model request in flight, '' when none
  screen: Screen; // where the walk last put the page
  acks: Route[];  // the page's auto-acks, held until AutoAck
  letAck: number; // acks the walk sent on purpose (Mark seen): let through
}

const WEEK_MS = 7 * 24 * 3600_000;

// The sidebar row: shown whether or not the session is open. A failure
// also pins a copy under "Needs you" that reads the same.
const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
// The row's Seen button, a sibling of the row (not on the pinned copy).
const seen = (c: Ctx) => c.page.locator(`.row-wrap:has(> button.row[data-id="${c.id}"]) > button.row-ack`).first();

const WORDS: Record<string, string> = { Idle: 'idle', Running: 'running', Done: 'done', Failed: 'failed' };

const histFile = (c: Ctx) => path.join(c.serve.home, '.bough', 'history', `${c.id}.jsonl`);

interface Entry { seq: number; at: string; kind: string; data?: Record<string, unknown> }

function entries(c: Ctx): Entry[] {
  return fs.readFileSync(histFile(c), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l) as Entry);
}

// serve's side of the marks, read the way internal/serve's digest reads
// them: the last entry that is news (a turn's summary and title are
// not), the last done, and the ack saved in meta.json.
function saved(c: Ctx): { newSinceAck: boolean; doneSinceAck: boolean; stale: boolean; testsFailed: boolean } {
  let ack = 0;
  try {
    const meta = JSON.parse(fs.readFileSync(path.join(c.serve.home, '.bough', 'serve', 'meta.json'), 'utf8'));
    ack = meta.sessions?.[c.id]?.ack ?? 0;
  } catch { /* no meta yet: nothing acked */ }
  const es = entries(c);
  const last = [...es].reverse().find((e) => e.kind !== 'turn-summary' && e.kind !== 'title');
  const done = [...es].reverse().find((e) => e.kind === 'done');
  // The last test run as the row reads one (serve's callTest): a bash
  // call naming a test runner; the walk only ever runs a failing one.
  const testsFailed = es.some((e) => e.kind === 'call' && e.data?.tool === 'bash' &&
    String(e.data?.cmd ?? '').includes('go test') && Number(e.data?.exit ?? 0) !== 0);
  return {
    newSinceAck: (last?.seq ?? 0) > ack,
    doneSinceAck: (done?.seq ?? 0) > ack,
    stale: last ? Date.now() - Date.parse(last.at) >= WEEK_MS : false,
    testsFailed,
  };
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const r = row(c);
  const label = (await r.getAttribute('aria-label')) ?? '';
  // "<title>, <why>[, not seen yet], <age> ago…"; why is the status word,
  // or a failure's reason ("Failed", "Tests failed; agent <word>").
  const parts = label.split(', ');
  const why = parts[1] ?? '';
  const tests = /^Tests failed; agent (\w+)/.exec(why);
  const status = tests ? WORDS[tests[1][0].toUpperCase() + tests[1].slice(1)] : WORDS[why];
  const hasSeen = (await seen(c).count()) > 0;
  const hash = await c.page.evaluate(() => window.location.hash);
  const on = (await r.getAttribute('aria-current')) === 'true';
  const screen = on && hash === `#/s/${c.id}` ? 'shown' : on && hash === `#/s/${c.id}/context` ? 'hidden' : 'closed';
  const s = saved(c);
  return {
    status: status ?? `unknown: ${label}`,
    testsFailed: tests ? true : why === 'Failed' && hasSeen ? s.testsFailed : false,
    newSinceAck: s.newSinceAck,
    doneSinceAck: s.doneSinceAck,
    stale: s.stale,
    screen,
    trouble: hasSeen ? (tests ? 'tests failed' : 'failed') : '',
    unseen: parts.includes('not seen yet'),
  };
}

// Waits until the transcript has not grown for a moment: the child
// writes a turn's summary and title just after its done, and a rewrite
// racing those appends would lose one.
async function settled(c: Ctx): Promise<void> {
  let size = -1;
  for (let i = 0; i < 100; i++) {
    const now = fs.statSync(histFile(c)).size;
    if (now === size) return;
    size = now;
    await new Promise((r) => setTimeout(r, 150));
  }
  throw new Error('transcript never settled');
}

async function prompt(c: Ctx, text: string): Promise<void> {
  const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text } });
  if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
}

modelTests<Ctx>({
  spec: 'unseen_trouble_ack',
  role: 'Session#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const c: Ctx = {
      page, serve, id: await serve.newSession(), decoy: await serve.newSession(), dir: controlDir(serve.home),
      turn: 0, held: '', screen: 'closed', acks: [], letAck: 0,
    };
    await page.route(`**/api/sessions/${c.id}/ack`, (route) => {
      if (c.letAck > 0) { c.letAck--; return route.continue(); }
      if (c.screen === 'shown') { c.acks.push(route); return; }
      return route.continue();
    });
    // Start on the decoy: "/" alone opens the most recent session itself.
    await page.goto(`${serve.url}/#/s/${c.decoy}`);
    return c;
  },

  actions: {
    // Prompt needs an idle session, which the spec never has open: the
    // POST another client would send.
    async Prompt(c) {
      const name = `t${String(++c.turn).padStart(4, '0')}`;
      queue(c.dir, name, { mode: 'block', text: `finished ${name}` });
      await prompt(c, `turn ${name}`);
      c.held = name;
      await waitTaken(c.dir, name);
    },
    // The held request answers with a failing test command; the engine
    // records it and asks again, and that request is held in turn.
    async TestFail(c) {
      const next = c.held + 'b';
      queue(c.dir, next, { mode: 'block', text: `finished ${next}` });
      // helpers/control's Turn has no bash; llm-control reads it from the same JSON.
      fs.writeFileSync(path.join(c.dir, c.held + '.release-tmp'), JSON.stringify({ bash: 'exit 3 # go test' }));
      fs.renameSync(path.join(c.dir, c.held + '.release-tmp'), path.join(c.dir, c.held + '.release'));
      c.held = next;
      await waitTaken(c.dir, next);
    },
    async Finish(c) { release(c.dir, c.held); c.held = ''; },
    async Fail(c) { releaseWith(c.dir, c.held, { mode: 'error', error: 'model says no' }); c.held = ''; },
    async Note(c) {
      if (c.screen === 'shown') {
        await c.page.locator('#composer').fill('!true');
        await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      } else {
        await prompt(c, '!true');
      }
    },
    async Expire(c) {
      await settled(c);
      const back = (_: string, base: string, frac: string | undefined, zone: string) =>
        `"at":"${new Date(Date.parse(base + zone) - WEEK_MS - 24 * 3600_000).toISOString().slice(0, 19)}${frac ?? ''}Z"`;
      const text = fs.readFileSync(histFile(c), 'utf8')
        .replace(/"at":"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d+)?(Z|[+-]\d\d:\d\d)"/g, back);
      // In place, not a rename: the child appends to this file by its open
      // descriptor (O_APPEND), and would go on writing to a replaced inode.
      fs.writeFileSync(histFile(c), text);
    },
    // Straight into the session's Context page: selected, its transcript
    // not on screen. Clicking the row first would show it, and a shown
    // unseen finish is acked on that render.
    async Open(c) {
      c.screen = 'hidden';
      await c.page.evaluate((id) => { window.location.hash = `#/s/${id}/context`; }, c.id);
    },
    // The row again: on a desktop the Context page's Back is hidden, and
    // the sidebar row is how a person returns to the transcript.
    async Show(c) {
      c.screen = 'shown';
      await row(c).click();
    },
    async Close(c) {
      c.screen = 'closed';
      await c.page.locator(`button.row[data-id="${c.decoy}"]`).first().click();
    },
    // The page sent its ack when the finish came on screen (held since);
    // letting it go is the step. None held means the page never sent it.
    async AutoAck(c) {
      const deadline = Date.now() + 10_000;
      while (!c.acks.length) {
        if (Date.now() > deadline) throw new Error('AutoAck: the page never acked the finish it shows');
        await c.page.clock.fastForward(1_000);
        await new Promise((r) => setTimeout(r, 50));
      }
      for (const r of c.acks.splice(0)) await r.continue();
    },
    // "Mark seen" in the thread header when it is on screen, else the
    // row's Seen.
    async MarkSeen(c) {
      c.letAck++;
      if (c.screen === 'shown') await c.page.locator('.thread-head').getByRole('button', { name: 'Mark seen', exact: true }).click();
      else {
        // Seen shows on hover of its row, as a person reaches for it.
        await seen(c).locator('..').hover();
        await seen(c).click();
      }
    },
    async DragDone(c) {
      const res = await c.serve.api.post(`/api/sessions/${c.id}/ack`);
      if (!res.ok()) throw new Error(`ack: ${res.status()} ${await res.text()}`);
    },
    // "end": the checker's stutter at a quiet end state (the spec has no
    // deadlock detection); the harness strips the role prefix it lacks,
    // leaving ''. Nothing happens.
    '': async () => {},
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.held) release(c.dir, c.held);
  },
});
