// go/tests/model/specs/turn_lifecycle.fizz in the browser: one web
// session prompted from the composer, its turn streamed, recorded, run
// through a native call and ended by done, an error or a dead child.
// Every generated path is walked through the page against a real serve
// whose model is llm-control; at every node readUiState must equal the
// spec's role state (recipe: go/tests/model/README.md).
//
// The spec's steps are the page's own, and most of them are over in
// milliseconds on their own. The walk stretches each one so it can be
// looked at, without changing what the page or the server does:
//
//   - POST /prompt is held in the browser (Send) until Accept lets it go
//     to the server as it was, or Refuse lets it meet a real 409 (the
//     session archived by another client while the request was out).
//   - A user-prompt-submit hook in the serve's HOME waits on a gate file
//     per send, so a send the server took is not recorded until Input
//     opens the gate. An adopted (interrupted) session has closed its
//     dangling turn by then, which is the "stopped" the spec says.
//   - The page's catch-up GETs of this session's transcript are held
//     until CatchUp: the page asked, the server has it, the screen does
//     not yet. The list poll is not held; status is read off it.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Request, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

interface Want { what: 'prompt' | 'say' | 'text' | 'calls'; text?: string; n?: number }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;       // llm-control's queue
  gates: string;     // the prompt hook's gate files
  n: number;         // names are unique across the walk
  text: string;      // the send in flight or last sent
  recorded: boolean; // its input is in history (Input ran)
  word: string;      // its unique word: the hook's gate and the input's marker
  held: string;      // the model request the turn holds, '' when none
  next: string;      // queued for the request after a native call
  gateF: string;     // the running native call's gate file
  post: Route | null;               // POST /prompt held by the walk
  since: Route[];                   // catch-up GETs held by the walk
  landing: Promise<unknown>[];      // catch-ups let go, not yet answered
  holding: boolean;                 // hold catch-ups (not the first load)
  frags: { text: string; rec: boolean }[]; // fragments streamed this turn
  callEnded: boolean;               // the native call's end is recorded
  calls: number;                    // native calls recorded so far
  want: Want[];                     // what the transcript must show once caught up
}

const name = (c: Ctx, p: string) => `${p}${String(++c.n).padStart(4, '0')}`;

// The composer's placeholder names the live turn it would steer.
const STEER = 'Steer the running turn…';

// The sidebar row: the list poll's view of the status. The label is
// "<title>, <state word or failure reason>, <age> ago…". A failure
// carries its reason instead of a word and the red second line; an
// interrupted session wears the red line too (serve reports it as
// trouble) but keeps its word.
const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const STATUS_WORDS: Record<string, string> = {
  Idle: 'idle', Running: 'running', Done: 'done', Stopped: 'stopped', Interrupted: 'interrupted',
};
// The header says the status word, Working/Sending/Waiting while live;
// a failure says "Failed" or its reason, which the spec calls Error.
const HEAD_WORDS = new Set(['Idle', 'Done', 'Stopped', 'Interrupted', 'Working', 'Sending', 'Waiting']);
const headWord = (text: string) => (HEAD_WORDS.has(text) ? text : 'Error');

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const label = (await row(c).getAttribute('aria-label')) ?? '';
  const failed = (await row(c).locator('.row-meta-bad').count()) > 0;
  const word = label.split(', ')[1] ?? '';
  const status = STATUS_WORDS[word] ?? (failed ? 'error' : `unknown: ${label}`);

  const dom = await c.page.evaluate((text) => {
    const thread = document.querySelector('.thread');
    const q = (sel: string) => [...(thread?.querySelectorAll(sel) ?? [])] as HTMLElement[];
    const pending = q('section.turn:not([data-turn])').filter((s) => (s.querySelector('.prompt-bubble')?.textContent ?? '').includes(text));
    const head = document.querySelector('.thread-head .head-main > .status') as HTMLElement | null;
    let headText = '';
    if (head) {
      const copy = head.cloneNode(true) as HTMLElement;
      copy.querySelectorAll('.visually-hidden').forEach((e) => e.remove());
      headText = (copy.textContent ?? '').trim();
    }
    const composer = document.querySelector('#composer') as HTMLTextAreaElement | null;
    return {
      pending: pending.length,
      sendingWord: pending.some((s) => s.querySelector('.turn-sending-state') !== null),
      notSent: q('.send-failed strong').some((e) => e.textContent === 'Not sent') || [...document.querySelectorAll('.send-failed strong')].some((e) => e.textContent === 'Not sent'),
      prompts: q('section.turn[data-turn] .prompt-bubble').map((e) => e.textContent ?? ''),
      says: q('.say:not(.stream-say)').map((e) => e.textContent ?? ''),
      stream: q('.stream-say, .thinking.stream-tip').map((e) => e.textContent ?? '').join('\n'),
      running: q('details.call-native .tool-running').length,
      recordedCalls: q('details.call-native[data-seq]').length,
      all: thread?.textContent ?? '',
      head: headText,
      placeholder: composer?.placeholder ?? '',
    };
  }, c.word || '\u0000');

  let send = 'none';
  if (dom.notSent) send = 'failed';
  // Taken and recorded read the same on the page (the send's row,
  // Waiting); which one it is is whether the walk let the hook record it.
  else if (dom.pending) send = dom.sendingWord ? 'sending' : c.recorded ? 'recorded' : 'accepted';

  const shown = c.frags.filter((f) => dom.stream.includes(f.text));
  const rec = shown.some((f) => f.rec), fresh = shown.some((f) => !f.rec);
  const stream = rec && fresh ? 'rec_fresh' : rec ? 'rec' : fresh ? 'fresh' : '';

  const call = dom.running ? (c.callEnded ? 'native_done' : 'native') : 'none';

  // Behind: something recorded that the screen does not show yet, or a
  // catch-up the page asked for that has not landed.
  const missing = c.want.some((w) => {
    switch (w.what) {
      case 'prompt': return !dom.prompts.some((p) => p.includes(w.text!));
      case 'say': return !dom.says.some((s) => s.includes(w.text!));
      case 'text': return !dom.all.includes(w.text!);
      case 'calls': return dom.recordedCalls < w.n!;
    }
  });
  const behind = missing || c.since.length > 0;

  return {
    status, send, stream, call, behind,
    head: headWord(dom.head),
    steer: dom.placeholder === STEER,
  };
}

// --- llm-control beyond helpers/control.ts: a fragment while held ---

async function streamFragment(dir: string, turn: string, text: string): Promise<void> {
  const dst = path.join(dir, turn + '.stream');
  fs.writeFileSync(dst + '-tmp', text);
  fs.renameSync(dst + '-tmp', dst);
  const deadline = Date.now() + 10_000;
  while (fs.existsSync(dst)) {
    if (Date.now() > deadline) throw new Error(`llm-control: ${turn} did not stream ${text}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}

// releaseWith's Turn has no call; the row reads one from the same JSON.
function releaseCall(dir: string, turn: string, body: Record<string, unknown>): void {
  const f = path.join(dir, turn + '.release');
  fs.writeFileSync(f + '-tmp', JSON.stringify(body));
  fs.renameSync(f + '-tmp', f);
}

async function until<T>(what: string, fn: () => Promise<T | undefined> | T | undefined, ms = 15_000): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v !== undefined && v !== false) return v as T;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

async function apiRow(c: Ctx): Promise<{ status: string; live: boolean }> {
  const res = await c.serve.api.get(`/api/sessions/${c.id}`);
  if (!res.ok()) throw new Error(`session: ${res.status()}`);
  return (await res.json()).session;
}

// The turn's end as the server has it: the row leaves running.
const ended = (c: Ctx) => until('the turn to end', async () => (await apiRow(c)).status !== 'running' || undefined);

// Another client archives the session (killing its child) and brings it
// back: the Go adapter's kill, and the one way to a real 409.
async function archive(c: Ctx): Promise<void> {
  const res = await c.serve.api.post(`/api/sessions/${c.id}/archive`, { data: {} });
  if (!res.ok()) throw new Error(`archive: ${res.status()}`);
  await until('the child to exit', async () => !(await apiRow(c)).live || undefined);
}
async function unarchive(c: Ctx): Promise<void> {
  const res = await c.serve.api.post(`/api/sessions/${c.id}/unarchive`, { data: {} });
  if (!res.ok()) throw new Error(`unarchive: ${res.status()}`);
}

function dropQueued(dir: string): void {
  for (const f of fs.readdirSync(dir)) if (f.endsWith('.json')) fs.rmSync(path.join(dir, f), { force: true });
}

async function endCall(c: Ctx): Promise<void> {
  fs.writeFileSync(c.gateF, '');
  c.gateF = '';
  await waitTaken(c.dir, c.next);
  c.held = c.next;
  c.next = '';
  c.callEnded = true;
  c.calls++;
  c.want.push({ what: 'calls', n: c.calls });
}

// A turn's end clears what was live on screen.
function closed(c: Ctx): void {
  c.frags = [];
  c.callEnded = false;
}

// Lets the page's catch-ups go to the server. Their timer runs on the
// page's clock: move it past the 120 ms debounce first, and give the
// page up to wait ms to ask.
async function land(c: Ctx, wait: number): Promise<void> {
  await c.page.clock.fastForward(1000);
  await until('the page to ask for the transcript', () => c.since.length > 0 || undefined, wait).catch(() => {});
  for (const r of c.since.splice(0)) {
    c.landing.push(c.page.waitForResponse((res) => res.request() === r.request()));
    await r.continue();
  }
  await Promise.all(c.landing.splice(0));
}

// The send on its way: the composer, and the Send button, as a person does.
async function send(c: Ctx): Promise<void> {
  await c.page.locator('#composer').fill(c.text);
  await c.page.getByRole('button', { name: 'Send', exact: true }).click();
  await until('POST /prompt', () => c.post !== null || undefined);
}

modelTests<Ctx>({
  spec: 'turn_lifecycle',
  role: 'Session#0',
  config: CONTROL_CONFIG,
  shared: true,
  reset: true,
  // Refuse's 409, which Chromium logs itself; the page logs nothing.
  allowConsole: /^Failed to load resource: the server responded with a status of 409 \(Conflict\)$/,

  async init(page, serve) {
    const gates = path.join(serve.home, 'gates');
    fs.mkdirSync(gates, { recursive: true });
    // Held until the walk writes gates/<the send's last word>. tools.bash
    // is synchronous in a hook; the wait is one step long.
    const hook = path.join(serve.home, '.bough', 'hooks', 'user-prompt-submit', 'gate.js');
    fs.mkdirSync(path.dirname(hook), { recursive: true });
    fs.writeFileSync(hook, [
      'const w = String(event.input).trim().split(/\\s+/).pop();',
      `tools.bash('while [ ! -e ${gates}/' + w + ' ]; do sleep 0.05; done');`,
      'return null;',
    ].join('\n'));

    const c: Ctx = {
      page, serve, id: await serve.newSession(), dir: controlDir(serve.home), gates, n: 0,
      text: '', word: '', recorded: false, held: '', next: '', gateF: '', post: null, since: [], landing: [], holding: false,
      frags: [], callEnded: false, calls: 0, want: [],
    };
    fs.mkdirSync(c.dir, { recursive: true });
    await page.route((u) => u.pathname === `/api/sessions/${c.id}/prompt`, (r) => { c.post = r; });
    await page.route((u) => u.pathname === `/api/sessions/${c.id}`, (r: Route, req: Request) => {
      if (req.method() !== 'GET' || !c.holding) return r.continue();
      c.since.push(r);
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await page.locator('#composer').waitFor();
    await page.locator('.thread-head .head-main > .status').waitFor();
    c.holding = true;
    return c;
  },

  actions: {
    // --- the person ---
    async Send(c) {
      c.word = name(c, 's');
      c.text = `send ${c.word}`;
      c.recorded = false;
      await send(c);
    },
    // The "Not sent" row's Retry: the same request again.
    async Retry(c) {
      await c.page.locator('.send-failed').getByRole('button', { name: 'Retry' }).click();
      await until('POST /prompt', () => c.post !== null || undefined);
    },
    async Edit(c) {
      await c.page.locator('.send-failed').getByRole('button', { name: 'Edit' }).click();
    },

    // --- the request ---
    async Accept(c) {
      c.held = name(c, 't');
      queue(c.dir, c.held, { mode: 'block' });
      const r = c.post!;
      c.post = null;
      const answered = c.page.waitForResponse((res) => res.url().endsWith(`/api/sessions/${c.id}/prompt`));
      await r.continue();
      const res = await answered;
      if (!res.ok()) throw new Error(`prompt: ${res.status()}`);
    },
    // Archived by another client while the request was out: a real 409.
    async Refuse(c) {
      await archive(c);
      const r = c.post!;
      c.post = null;
      const answered = c.page.waitForResponse((res) => res.url().endsWith(`/api/sessions/${c.id}/prompt`));
      await r.continue();
      const res = await answered;
      if (res.status() !== 409) throw new Error(`prompt to an archived session: want 409, got ${res.status()}`);
      await unarchive(c);
      // The kill and the unarchive are the walk's doing, not the page's
      // news: what they made the page ask for lands before the read.
      await land(c, 1000);
    },

    // --- the child ---
    async Input(c) {
      fs.writeFileSync(path.join(c.gates, c.word), '');
      await waitTaken(c.dir, c.held);
      await until('running', async () => (await apiRow(c)).status === 'running' || undefined);
      c.recorded = true;
      c.want.push({ what: 'prompt', text: c.text });
    },
    async Delta(c) {
      const t = name(c, 'f');
      await streamFragment(c.dir, c.held, `${t} `);
      c.frags.push({ text: t, rec: false });
    },
    // The entry the fragments built, and a call to a tool that does not
    // exist (recorded at once, never a running row) so the turn goes on.
    async Reply(c) {
      const next = name(c, 't');
      queue(c.dir, next, { mode: 'block' });
      releaseCall(c.dir, c.held, { text: `reply ${next}`, call: { name: 'no_such_tool' } });
      for (const f of c.frags) f.rec = true;
      c.want.push({ what: 'say', text: `reply ${next}` });
      await waitTaken(c.dir, next);
      c.held = next;
    },
    async NativeCall(c) {
      c.next = name(c, 't');
      c.gateF = path.join(c.serve.home, `${c.next}.gate`);
      queue(c.dir, c.next, { mode: 'block' });
      releaseCall(c.dir, c.held, { call: { name: 'bash', args: { command: `while [ ! -e ${c.gateF} ]; do sleep 0.05; done; echo gate open` } } });
      c.held = '';
    },
    CallEnd: (c) => endCall(c),
    async Finish(c) {
      if (c.gateF) await endCall(c);
      releaseWith(c.dir, c.held, { mode: 'ok', text: `finished ${c.held}` });
      c.want.push({ what: 'say', text: `finished ${c.held}` });
      c.held = '';
      await ended(c);
      closed(c);
    },
    async Fail(c) {
      releaseWith(c.dir, c.held, { mode: 'error', error: 'model says no' });
      c.want.push({ what: 'text', text: 'model says no' });
      c.held = '';
      await ended(c);
      closed(c);
    },
    // The child dies mid-turn (the archive kill, undone at once).
    async Crash(c) {
      await archive(c);
      await unarchive(c);
      if (c.gateF) fs.writeFileSync(c.gateF, '');
      dropQueued(c.dir);
      c.held = c.next = c.gateF = '';
      closed(c);
    },

    // --- the page ---
    // The catch-up the page asked for lands.
    CatchUp: (c) => land(c, 3000),
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    c.holding = false;
    for (const r of c.since.splice(0)) await r.continue().catch(() => {});
    if (c.post) await c.post.abort().catch(() => {});
    if (c.word) fs.writeFileSync(path.join(c.gates, c.word), '');
    if (c.gateF) fs.writeFileSync(c.gateF, '');
    if (c.held) releaseWith(c.dir, c.held, { mode: 'ok', text: 'cleanup' });
    if (c.next) releaseWith(c.dir, c.next, { mode: 'ok', text: 'cleanup' });
  },
});
