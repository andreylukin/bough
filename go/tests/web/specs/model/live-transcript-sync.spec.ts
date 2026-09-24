// Model-based browser spec for go/tests/model/specs/live_transcript_sync.fizz
// (recipe: go/tests/model/README.md): how an open thread stays equal to
// the session's transcript on disk while its event stream delivers,
// drops and reconnects, and its catch-ups land, fail and race the first
// read. Every generated path is walked in Chromium against a real serve
// (one per worker, llm-control as the model); at every node the spec's
// Sync#0 state must equal what the page shows.
//
// The spec interleaves the page's own steps (a response landing, a timer
// firing, the stream delivering a frame), so the walk owns the page's
// network and clock, and nothing else:
//   - the clock is paused: the 120 ms catch-up timer (or its 4..30 s
//     backoff) fires only when CatchUpFire moves it. modelTests in
//     helpers/model.ts moves the clock 5 s before every read, which would
//     fire the timer at a node where the spec still has it armed, so
//     this flow walks its paths itself (same shape, same invariants).
//   - GET /api/sessions/{id}[?since=] is held with page.route: InitialServe
//     reads the server, InitialOk / CatchUpOk hand the page that real
//     response, CatchUpFail hands it a failure.
//   - EventSource is wrapped by an init script (below): frames the real
//     server sends wait in an inbox until Deliver hands them to the
//     page's own handler. A (re)connect's replay is handed over at once,
//     as the spec's replay() does. The spec's full-channel drop is the
//     wrapper closing the real stream before the frame is sent: to the
//     page a dropped subscriber and a dropped stream are one thing, and
//     the Go adapter reads it the same way (mbt/live_transcript_sync_test.go).
//
// What each field is read from:
//   sel, loaded, lines, paused  the DOM (hash, the transcript region, the
//                               "Updates paused" status), never the API
//   conn, inbox                 the EventSource wrapper: what the page
//                               has not been handed yet
//   get, fetch                  the held requests, as the page sent them
//   timer                       a recorded frame handed to the page since
//                               the last catch-up started; CatchUpFire
//                               then requires the page's request to come
//   disk, ring                  the server's side, read after the step
//                               that changes it, as another client would
import * as fs from 'fs';
import * as http from 'http';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { loadPaths, roleState } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

const SPEC = 'live_transcript_sync';
const ROLE = 'Sync#0';
const QUIET_MS = 250;

interface Line { seq: number; kind: string; text?: string }
interface Frame { data: string; id: string | null }
type Abs = { kind: string; seq: number; id: number };

interface Held { sess: number; route: Route; since: number; stale?: boolean; served?: { entries: Line[]; response: Awaited<ReturnType<Route['fetch']>> } }

interface Ctx {
  page: Page;
  serve: Serve;
  ids: [string, string];   // [live session #0, finished session #1]
  held: string;            // the llm-control turn of #0, '' once released
  says: number;
  base0: number;           // last history seq of #0 before the walk
  baseEv0: number;         // last supervisor seq of #0 before the walk
  seqs0: Set<number>;      // #0's recorded entry, once recorded
  last0: number;
  disk: number;
  ring: Abs[];
  get: Held | null;
  fetch: Held | null;
  catching: boolean;       // the next GET is a catch-up
  timer: boolean;
  routes: Route[];         // every request left held, released in cleanup
}

// One finished session per worker's serve: nothing in the model writes it.
const statics = new Map<string, { id: string; seqs: Set<number>; last: number }>();
let turns = 0;

// --- the page's EventSource, held by the walk -------------------------

// Runs in the page before app.tsx. Only a session's event stream is
// wrapped; the page sees an object with the same onmessage/close shape.
function eventSourceShim(): void {
  const Real = window.EventSource;
  type F = { data: string; id: string | null };
  const w = window as unknown as { __lts: Record<string, unknown> };
  const lts: Record<string, unknown> & { cur: Shim | null } = { cur: null };
  w.__lts = lts;
  class Shim {
    url: string;
    onmessage: ((m: MessageEvent) => void) | null = null;
    onerror: unknown = null;
    onopen: unknown = null;
    real: EventSource | null = null;
    conn = 'live';
    replaying = true;
    buf: F[] = [];
    items: F[][] = [];
    received = 0;
    last = '';
    constructor(url: string) {
      this.url = url;
      lts.cur = this;
      this.open();
    }
    get readyState() { return this.real ? this.real.readyState : 2; }
    open() {
      const real = new Real(this.url);
      this.real = real;
      this.conn = 'live';
      this.replaying = true;
      this.last = '';
      real.onmessage = (m) => {
        if (this.real !== real) return;
        this.received++;
        // lastEventId carries over a frame without an id line; a change
        // is what an id line looks like from here.
        const f = { data: m.data, id: m.lastEventId !== this.last ? m.lastEventId : null };
        this.last = m.lastEventId;
        let kind = '';
        try { kind = JSON.parse(m.data).kind; } catch { /* dropped by the page as well */ }
        // An activity label is outside the model: the page takes it at once.
        if (this.replaying || kind === 'activity') this.hand(f);
        else this.buf.push(f);
      };
    }
    hand(f: F) { this.onmessage?.(new MessageEvent('message', { data: f.data, lastEventId: f.id ?? '' })); }
    close() {
      this.real?.close();
      this.real = null;
      this.conn = 'closed';
      if (lts.cur === this) lts.cur = null;
    }
  }
  const ctor = function (this: unknown, url: string | URL, init?: EventSourceInit) {
    const u = String(url);
    if (/\/api\/sessions\/[^/]+\/events$/.test(u)) return new Shim(u);
    return new Real(u, init);
  } as unknown as typeof EventSource;
  Object.assign(ctor, { CONNECTING: 0, OPEN: 1, CLOSED: 2 });
  window.EventSource = ctor;
  lts.state = () => {
    const c = lts.cur;
    if (!c) return { url: '', conn: 'none', items: [], buf: [], received: 0, open: false };
    return { url: c.url, conn: c.conn, items: c.items, buf: c.buf, received: c.received, open: c.real?.readyState === 1 };
  };
  lts.endReplay = () => { if (lts.cur) lts.cur.replaying = false; };
  lts.take = () => { const c = lts.cur!; if (c.buf.length) c.items.push(c.buf); c.buf = []; };
  lts.deliver = () => { const c = lts.cur!; for (const f of c.items.shift() ?? []) c.hand(f); };
  lts.drop = () => { const c = lts.cur!; c.real?.close(); c.real = null; c.conn = 'dropped'; };
  lts.reconnect = () => { lts.cur!.open(); };
}

type ShimState = { url: string; conn: string; items: Frame[][]; buf: Frame[]; received: number; open: boolean };
const shim = (c: Ctx) => c.page.evaluate(() => ((window as unknown as { __lts: { state(): unknown } }).__lts.state() as ShimState));
const shimDo = (c: Ctx, op: 'endReplay' | 'take' | 'deliver' | 'drop' | 'reconnect') =>
  c.page.evaluate((o) => ((window as unknown as { __lts: Record<string, () => void> }).__lts[o]()), op);

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until<T>(what: string, f: () => Promise<T | undefined | null | false>, ms = 15_000): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await f();
    if (v) return v;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(25);
  }
}

// A (re)connect: the stream opens and replays the ring; once it is quiet
// the replay is over and later frames wait for Deliver.
async function replayed(c: Ctx, sess: number): Promise<void> {
  const want = `/api/sessions/${c.ids[sess]}/events`;
  let seen = -1;
  let since = Date.now();
  await until(`the ${sess === 0 ? 'live' : 'finished'} session's replay`, async () => {
    const s = await shim(c);
    if (!s.url.endsWith(want) || !s.open) return false;
    if (s.received !== seen) { seen = s.received; since = Date.now(); return false; }
    return Date.now() - since >= QUIET_MS;
  });
  await shimDo(c, 'endReplay');
}

// --- what a frame is in the spec ---------------------------------------

function abs(c: Ctx, sess: number, f: Frame): Abs | null {
  const ev = JSON.parse(f.data) as { kind: string; seq: number };
  if (!ev.seq) return { kind: 'delta', seq: 0, id: f.id === null ? 0 : -1 };
  if (ev.kind === 'activity' || (sess === 0 && ev.seq <= c.baseEv0)) return null;
  return { kind: 'rec', seq: 1, id: f.id === String(ev.seq) ? 1 : -1 };
}

// One step's frames are one frame in the spec: its record, or its delta
// run. A frame the step should not have sent shows beside it.
function item(c: Ctx, frames: Frame[]): Abs[] {
  const xs = frames.map((f) => abs(c, 0, f)).filter((x): x is Abs => x !== null);
  const rec = xs.filter((x) => x.kind === 'rec');
  const bad = xs.filter((x) => x.kind === 'delta' && x.id !== 0);
  if (rec.length) return [...bad, rec.find((x) => x.id !== 1) ?? rec[0]];
  return [...bad, ...(xs.length ? [{ kind: 'delta', seq: 0, id: 0 }] : [])];
}

// --- the server's side, as another client reads it ---------------------

async function transcript(c: Ctx, id: string): Promise<Line[]> {
  const res = await c.serve.api.get(`/api/sessions/${id}`);
  if (!res.ok()) throw new Error(`GET ${id}: ${res.status()}`);
  return (await res.json()).entries as Line[];
}

// Reads until two reads a quiet apart agree: a turn's summary and title
// land after its done.
async function settled(c: Ctx, id: string): Promise<Line[]> {
  let prev: Line[] | null = null;
  for (let i = 0; i < 100; i++) {
    const ls = await transcript(c, id);
    if (prev && prev.length === ls.length) return ls;
    prev = ls;
    await sleep(QUIET_MS);
  }
  throw new Error(`transcript of ${id} never settled`);
}

// The ring, as a new subscriber is replayed it.
function probe(s: Serve, id: string): Promise<Frame[]> {
  return new Promise((resolve, reject) => {
    const got: Frame[] = [];
    let quiet: NodeJS.Timeout | undefined;
    const req = http.get(`${s.url}/api/sessions/${id}/events`, { headers: { Authorization: `Bearer ${s.token}` } }, (res) => {
      let buf = '';
      let id: string | null = null;
      let data = '';
      const done = () => { req.destroy(); resolve(got); };
      quiet = setTimeout(done, QUIET_MS * 2);
      res.on('data', (d) => {
        buf += String(d);
        let i;
        while ((i = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, i);
          buf = buf.slice(i + 1);
          if (line === '') { if (data) got.push({ data, id }); id = null; data = ''; }
          else if (line.startsWith('id: ')) id = line.slice(4);
          else if (line.startsWith('data: ')) data += line.slice(6);
        }
        clearTimeout(quiet);
        quiet = setTimeout(done, QUIET_MS);
      });
    });
    req.on('error', (e) => { clearTimeout(quiet); if (!req.destroyed || got.length === 0) reject(e); });
  });
}

// The spec's count of entries: 0 none, 1 the whole modelled entry, -1
// anything else.
function count(c: Ctx, sess: number, lines: Line[]): number {
  const st = statics.get(c.serve.url)!;
  const lo = sess === 0 ? c.base0 : 0;
  const seqs = sess === 0 ? c.seqs0 : st.seqs;
  let n = 0;
  for (const l of lines) {
    if (l.seq <= lo) continue;
    if (!seqs.has(l.seq)) return -1;
    n++;
  }
  return n === 0 ? 0 : n === seqs.size ? 1 : -1;
}

function cursor(c: Ctx, sess: number, since: number): number {
  const st = statics.get(c.serve.url)!;
  const [lo, hi] = sess === 0 ? [c.base0, c.last0] : [0, st.last];
  return since <= lo ? 0 : since === hi ? 1 : -1;
}

// --- the page's reads, held ---------------------------------------------

async function onRead(c: Ctx, route: Route): Promise<void> {
  const u = new URL(route.request().url());
  const id = decodeURIComponent(u.pathname.split('/')[3]);
  const sess = c.ids.indexOf(id);
  if (sess < 0) { await route.continue(); return; }
  c.routes.push(route);
  const h: Held = { sess, route, since: Number(u.searchParams.get('since') ?? 0) };
  if (c.catching) {
    if (c.fetch) throw new Error('a second catch-up while one is in flight');
    c.catching = false;
    c.fetch = h;
    return;
  }
  // A new first read replaces the old one, whose answer the page ignores
  // (`live` is false): let it land.
  const old = c.get;
  c.get = h;
  if (old) await settle(c, old);
}

async function settle(c: Ctx, h: Held): Promise<void> {
  c.routes = c.routes.filter((r) => r !== h.route);
  if (h.served) await h.route.fulfill({ response: h.served.response });
  else await h.route.continue();
}

async function serveRead(h: Held): Promise<void> {
  const response = await h.route.fetch();
  const body = await response.json();
  h.served = { entries: body.entries as Line[], response };
}

// --- the page's state ---------------------------------------------------

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const dom = await c.page.evaluate(() => {
    const t = document.querySelector('.transcript');
    let text = '';
    if (t) {
      // A stream preview is not a line: the recorded entry replaces it.
      const cl = t.cloneNode(true) as HTMLElement;
      cl.querySelectorAll('.stream-say, .stream-tip').forEach((e) => e.remove());
      text = cl.textContent ?? '';
    }
    const p = document.querySelector('.rt-paused');
    return { hash: location.hash, text, paused: Boolean(p && (p as HTMLElement).offsetParent !== null && /Updates paused/.test(p.textContent ?? '')) };
  });
  const m = /^#\/s\/([^/?]+)/.exec(dom.hash);
  const sel = m ? c.ids.indexOf(m[1]) : -1;
  const prompt = [`turn ${turnName(c)}`, 'static turn'];
  const reply = [`recorded ${turnName(c)}`, 'static reply'];
  const lines: { sess: number; seq: number }[] = [];
  for (const s of [0, 1]) {
    if (dom.text.includes(reply[s])) lines.push({ sess: s, seq: 1 });
    else if (s !== sel && dom.text.includes(prompt[s])) lines.push({ sess: s, seq: -1 }); // another session's transcript
  }
  const sh = await shim(c);
  const inbox: Abs[] = [];
  for (const it of sh.items) inbox.push(...item(c, it));
  // Frames that came outside any step: none are expected.
  if (sh.buf.length) inbox.push(...item(c, sh.buf));
  const conn = sh.conn === 'closed' ? 'none' : sh.conn;
  return {
    disk: c.disk,
    ring: c.ring,
    sel,
    conn,
    inbox,
    lines,
    loaded: sel >= 0 && dom.text.includes(prompt[sel]),
    get: c.get ? { sess: c.get.sess, snap: c.get.served ? count(c, c.get.sess, c.get.served.entries) : -1 } : { sess: -1, snap: -1 },
    fetch: c.fetch ? { sess: c.fetch.sess, since: cursor(c, c.fetch.sess, c.fetch.since), live: !c.fetch.stale } : { sess: -1, since: 0, live: false },
    timer: c.timer,
    paused: dom.paused,
  };
}

const name = (n: number) => `w${String(n).padStart(5, '0')}`;
const turnOf = new WeakMap<Ctx, string>();
const turnName = (c: Ctx) => turnOf.get(c)!;

// --- actions -------------------------------------------------------------

// The spec's fanout(): whether this step's frames reach the page's
// stream. A full channel (CAP = 1) drops the subscriber first.
async function fanning(c: Ctx): Promise<boolean> {
  const s = await shim(c);
  if (!s.url.endsWith(`/api/sessions/${c.ids[0]}/events`) || s.conn !== 'live') return false;
  if (s.items.length >= 1) { await shimDo(c, 'drop'); return false; }
  return true;
}

function armFrom(c: Ctx, sess: number, frames: Frame[]): void {
  if (frames.some((f) => abs(c, sess, f)?.kind === 'rec')) c.timer = true;
}

async function select(c: Ctx, sess: number): Promise<void> {
  const row = c.page.locator(`button.row[data-id="${c.ids[sess]}"]`).first();
  await row.click();
  await until('the first read', async () => c.get?.sess === sess);
  await replayed(c, sess);
}

async function replayArms(c: Ctx, sess: number): Promise<void> {
  // What the page was just handed on (re)connect: the ring.
  armFrom(c, sess, await probe(c.serve, c.ids[sess]));
}

const actions: Record<string, (c: Ctx) => Promise<void>> = {
  async Ephemeral(c) {
    const sub = await fanning(c);
    c.says++;
    const dir = controlDir(c.serve.home);
    const f = `${dir}/${c.held}.say-${String(c.says).padStart(6, '0')}`;
    fs.writeFileSync(f + '-tmp', `say ${c.says} `);
    fs.renameSync(f + '-tmp', f);
    await until(`say ${c.says} streamed`, async () => fs.existsSync(`${dir}/${c.held}.said-${String(c.says).padStart(6, '0')}`));
    if (!sub) { await sleep(QUIET_MS); return; }
    await until("the say's delta", async () => (await shim(c)).buf.some((x) => !JSON.parse(x.data).seq));
    await sleep(QUIET_MS);
    await shimDo(c, 'take');
  },

  async Record(c) {
    const sub = await fanning(c);
    release(controlDir(c.serve.home), c.held);
    c.held = '';
    await until('the turn to finish', async () => (await transcript(c, c.ids[0])).some((l) => l.seq > c.base0 && l.kind === 'done'));
    const ls = await settled(c, c.ids[0]);
    c.seqs0 = new Set(ls.filter((l) => l.seq > c.base0).map((l) => l.seq));
    c.last0 = Math.max(c.base0, ...c.seqs0);
    c.disk = ls.filter((l) => l.seq > c.base0 && l.kind === 'done').length;
    if (sub) {
      await until("the turn's done on the stream", async () => (await shim(c)).buf.some((x) => JSON.parse(x.data).kind === 'done'));
      await sleep(QUIET_MS);
      await shimDo(c, 'take');
    }
    const ring = (await probe(c.serve, c.ids[0])).map((f) => abs(c, 0, f)).filter((x): x is Abs => x !== null);
    const deltas = ring.filter((x) => x.kind === 'delta');
    const recs = ring.filter((x) => x.kind === 'rec');
    c.ring = [...deltas, ...recs.slice(-1)]; // RING = 1: the newest record
  },

  async Open(c) {
    await select(c, 0);
    await replayArms(c, 0);
  },

  async Switch(c) {
    const sel = (await readUiState(c)).sel as number;
    c.timer = false;
    await select(c, 1 - sel);
    // A catch-up still held is the old run's now: the page must ignore
    // its answer whenever it lands.
    if (c.fetch) c.fetch.stale = true;
    await replayArms(c, 1 - sel);
  },

  async InitialServe(c) {
    await serveRead(c.get!);
  },

  async InitialOk(c) {
    const h = c.get!;
    c.get = null;
    await settle(c, h);
  },

  async Deliver(c) {
    const it = (await shim(c)).items[0];
    await shimDo(c, 'deliver');
    armFrom(c, 0, it);
  },

  async Reconnect(c) {
    const sel = (await readUiState(c)).sel as number;
    await shimDo(c, 'reconnect');
    await replayed(c, sel);
    await replayArms(c, sel);
  },

  async CatchUpFire(c) {
    c.catching = true;
    // Past the longest backoff: the one timer the page has armed fires.
    await c.page.clock.runFor(30_000);
    await until('the catch-up the timer starts', async () => c.fetch !== null, 5_000);
    c.timer = false;
  },

  async Retry(c) {
    c.catching = true;
    await c.page.locator('.rt-paused').getByRole('button', { name: 'Retry', exact: true }).click();
    await until('the catch-up Retry starts', async () => c.fetch !== null, 5_000);
    c.timer = false;
  },

  async CatchUpOk(c) {
    const h = c.fetch!;
    c.fetch = null;
    await serveRead(h);
    await settle(c, h);
  },

  async CatchUpFail(c) {
    const h = c.fetch!;
    c.fetch = null;
    c.routes = c.routes.filter((r) => r !== h.route);
    // The network failing: a gateway's answer, which the page reads as
    // it would any failed read.
    await h.route.fulfill({ status: 502, contentType: 'application/json', body: '{"error":"bad gateway"}' });
    if (!h.stale) c.timer = true;
  },
};

// --- the walk ------------------------------------------------------------

async function prepare(page: Page, serve: Serve): Promise<Ctx> {
  const dir = controlDir(serve.home);
  if (!statics.has(serve.url)) {
    queue(dir, 'a0000', { mode: 'ok', text: 'static reply' });
    const id = await serve.newSession('static turn');
    const deadline = Date.now() + 20_000;
    let ls: Line[] = [];
    for (;;) {
      const res = await serve.api.get(`/api/sessions/${id}`);
      const body = await res.json();
      ls = body.entries;
      if (body.session.status === 'done' && ls.some((l: Line) => l.kind === 'done')) break;
      if (Date.now() > deadline) throw new Error('the static turn never finished');
      await sleep(50);
    }
    await sleep(QUIET_MS * 2);
    ls = (await (await serve.api.get(`/api/sessions/${id}`)).json()).entries;
    statics.set(serve.url, { id, seqs: new Set(ls.map((l) => l.seq)), last: Math.max(...ls.map((l) => l.seq)) });
  }
  const st = statics.get(serve.url)!;
  const t = name(++turns);
  queue(dir, t, { mode: 'block', text: `recorded ${t}` });
  const live = await serve.newSession(`turn ${t}`);
  await waitTaken(dir, t);
  const c: Ctx = {
    page, serve, ids: [live, st.id], held: t, says: 0, base0: 0, baseEv0: 0, seqs0: new Set(), last0: 0,
    disk: 0, ring: [], get: null, fetch: null, catching: false, timer: false, routes: [],
  };
  turnOf.set(c, t);
  const ls = await settled(c, live);
  c.base0 = Math.max(0, ...ls.map((l) => l.seq));
  c.last0 = c.base0;
  for (const f of await probe(serve, live)) c.baseEv0 = Math.max(c.baseEv0, (JSON.parse(f.data) as { seq: number }).seq);
  return c;
}

// Every node, whatever the flow (as in helpers/model.ts): the page does
// not scroll sideways, the state's carrier is visible, nothing logged an error.
async function invariants(page: Page, carrier: ReturnType<Page['locator']>, errors: string[], where: string): Promise<void> {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(carrier, `${where}: status not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

// The carrier: the open thread's transcript, or with none open the live
// session's row it is opened from.
const status = (c: Ctx, sel: number) => sel >= 0 ? c.page.getByRole('region', { name: 'Transcript' }) : c.page.locator(`button.row[data-id="${c.ids[0]}"]`).first();

// The live session's transcript, for TestHistoryTraces in go/tests/model/mbt
// to replay on the graph (what helpers/model.ts does for modelTests).
function saveTranscript(serve: Serve, id: string, title: string): void {
  const root = process.env.MODEL_TRACE_DIR;
  if (!root) return;
  const dir = path.join(root, SPEC);
  fs.mkdirSync(dir, { recursive: true });
  const src = path.join(serve.home, '.bough', 'history', id + '.jsonl');
  // Capped below the 255-byte file name limit, as in helpers/model.ts.
  const slug = title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 120);
  if (fs.existsSync(src)) fs.copyFileSync(src, path.join(dir, `${slug}-${id}.jsonl`));
}

// One serve per worker (its config forces a worker of its own, so top level).
test.use({ workerServeOpts: { config: CONTROL_CONFIG } });

test.describe(`model: ${SPEC}`, () => {

  loadPaths(SPEC).forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => s.action.slice(ROLE.length + 1)).join(' → ');
    test(`path ${i}: ${walk}`, async ({ sharedServe: serve, page }, info) => {
      test.setTimeout(120_000);
      const errors: string[] = [];
      // Chromium logs the failure CatchUpFail injects (a 502, which serve
      // itself never answers); that line is the walk's, not the page's.
      page.on('console', (m) => { if (m.type() === 'error' && !/status of 502 \(Bad Gateway\)/.test(m.text())) errors.push(m.text()); });
      page.on('pageerror', (e) => errors.push(String(e)));
      const c = await prepare(page, serve);
      await page.addInitScript(eventSourceShim);
      await page.route(/\/api\/sessions\/[^/?]+(\?.*)?$/, (r) => {
        if (r.request().method() !== 'GET') return r.continue();
        return onRead(c, r);
      });
      await page.clock.install();
      await page.clock.pauseAt(Date.now() + 1000);
      // Nothing selected: a page beside the list ("/" alone opens a session itself).
      await page.goto(`${serve.url}/#/hooks`);
      await expect(page.locator(`button.row[data-id="${c.ids[0]}"]`).first()).toBeVisible();
      try {
        for (const [n, step] of trace.entries()) {
          const act = step.action === 'Init' ? 'Init' : step.action.slice(ROLE.length + 1);
          const where = `step ${n} (${act})`;
          if (n > 0) {
            const f = actions[act];
            if (!f) throw new Error(`${SPEC}: no action for ${step.action}`);
            try { await f(c); } catch (e) { throw new Error(`${where}: ${(e as Error).message}`); }
          }
          const want = roleState(ROLE, step.state);
          // Until the first read lands the thread shows no lines at all
          // (Thread renders none while loading), though a catch-up may
          // already have added some to the page's state; those still
          // count, through the cursor a later catch-up sends (fetch.since).
          const shown = { ...want, lines: want.loaded ? want.lines : [] };
          await expect.poll(() => readUiState(c), { message: `${where}: state`, timeout: 5_000 }).toEqual(shown);
          await invariants(page, status(c, want.sel as number), errors, where);
        }
      } finally {
        if (c.held) release(controlDir(serve.home), c.held);
        for (const r of c.routes) await r.continue().catch(() => {});
        await page.unrouteAll({ behavior: 'ignoreErrors' });
        saveTranscript(serve, c.ids[0], info.title);
      }
    });
  });
});
