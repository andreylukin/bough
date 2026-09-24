// go/tests/model/specs/background_bash_jobs.fizz in the browser (recipe:
// go/tests/model/README.md): one session on engine-unreal, the model
// (llm-control) starting a background bash job with or without an until
// pattern, its notices waking the idle agent or queueing in the turn in
// flight, and Stop, the Work dialog's Stop and the step budget cutting in.
// The Go twin is go/tests/model/mbt/background_bash_jobs_test.go; this
// file drives the same steps, the person's through the page:
//
//   Prompt    typed in the composer, Send
//   Stop      the composer's Stop on a running turn; with no turn running
//             the page has no Stop, so it is serve's POST /interrupt, as
//             another client would send it
//   KillJob   the Work dialog's "Stop Job 1"
//
// The walks are the spec's steerable walks (steerable() below, the Go
// adapter's bbjSteerable): every link the real system can be steered
// through, from Init. The spec splits steps the real system takes in one
// breath, so as in Go the walk keeps the spec's record (Shadow) and makes
// a job event real at its NoticeDelivered, a JobKill's notice lands on its
// own, Stop and the budget stop are waited out at once (`ahead`), and a
// limit is made real as a kill. The generated walks that leave the
// steerable part (two notices at once, a notice pending at Stop, ...) are
// the Go adapter's to count; walked here they would compare the record
// with itself.
//
// What is read off the page at every node, and what is not:
//
//   child     the sidebar row's data-live
//   turn      the row's status word; a running turn opened by a notice
//             is a section with the wake banner (.job-wake)
//   last      the last closed turn's footer: Stopped, else its kind
//   wakes     the wake banners
//   job,typed the Work button (running) and Job 1's row: its state word
//             and the reason a killed job ended
//   queued, model, lost, quit_log
//             the notices on the page: a wake banner's (the job's row
//             for a finish, the "matched" banner for a match), the job
//             notes inside a turn (the job's row, the "matched" block),
//             and the notes in no turn at all (the exit's). Which of the
//             in-turn notes reached the model is the model's side, read
//             off the requests llm-control took, as in Go: the page cannot
//             know what the model was sent.
//   until, pending, waits, prompts, stops, budget
//             the record: until is the model's own argument and the page
//             shows no pattern, pending and waits are inside the child,
//             the rest are bounds on the walk.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, TestInfo } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken, type Turn } from '../../helpers/control';
import { roleState } from '../../helpers/model';
import { expect, test, type Serve } from '../../helpers/serve';
import { loadGraph, type Graph, type Step } from '../../model/graph';

const SPEC = 'background_bash_jobs';
const ROLE = 'Session#0';
// The engine's step budget: BudgetStop pads a turn up to it. It sits
// above any turn a walk takes on its own.
const MAX_STEPS = 24;
const CONFIG = CONTROL_CONFIG + `- id: loop\n  plugin: engine-unreal\n  config:\n    max_steps: ${MAX_STEPS}\n`;
const MATCH = 'BBJ-MATCH';
const BOUND = 30_000;

type State = Record<string, unknown>;

// ---- the walks ----

// steers is the Go adapter's detach rules (bbjSteers) as a predicate on a
// link out of a spec state: false where the real system cannot be put.
function steers(s: State, landing: boolean, action: string): boolean {
  const n = (f: string) => ((s[`${ROLE}.${f}`] as unknown[]) ?? []).length;
  if (landing && action !== 'NoticeDelivered') return false;
  switch (action) {
    case 'Stop': case 'JobKill': return n('pending') === 0;
    case 'OutputMatchesUntil': case 'JobExits': case 'LimitExpires': return s[`${ROLE}.child`] !== 'exiting';
    case 'NoticeDelivered': return s[`${ROLE}.turn`] === 'cancelling' || n('pending') <= 1;
    case 'CancelCloses': return s[`${ROLE}.waits`] !== true || n('pending') <= 1;
    case 'BudgetStop': return n('queued') === 0;
  }
  return true;
}

// steerable is bbjSteerable: walks from Init over the steerable links,
// each going to the nearest state (or link, for MODEL_COVER=transitions)
// not yet covered, at most 50 steps.
function steerable(g: Graph, cover: 'states' | 'transitions'): Step[][] {
  const out: number[][] = g.nodes.map(() => []);
  g.links.forEach((l, i) => out[l.src].push(i));
  // settle follows fork links (BashBackground's until) to settled nodes.
  const settle = (n: number): number[] => {
    if (g.nodes[n].name === 'yield') return [n];
    const ys = out[n].filter((i) => g.links[i].type !== 'action').flatMap((i) => settle(g.links[i].dest));
    return ys.length ? ys : [n];
  };
  interface Edge { name: string; dest: number }
  const edges: Edge[][] = g.nodes.map((_, n) => out[n].filter((i) => g.links[i].type === 'action')
    .flatMap((i) => settle(g.links[i].dest).map((y) => ({ name: g.links[i].name.replace(`${ROLE}.`, ''), dest: y }))));
  // A product node: a graph node, and whether a JobKill's notice is on
  // its way (only NoticeDelivered may follow).
  interface P { n: number; landing: boolean }
  const pk = (p: P) => `${p.n}/${p.landing}`;
  const next = (p: P, e: Edge): P | null => (steers(g.nodes[p.n].state, p.landing, e.name) ? { n: e.dest, landing: e.name === 'JobKill' } : null);
  const key = (p: P, e: Edge) => `${p.n} ${e.name} ${e.dest}`;
  const want = new Set<string>();
  const seen = new Set<string>([pk({ n: 0, landing: false })]);
  for (let frontier: P[] = [{ n: 0, landing: false }]; frontier.length;) {
    const more: P[] = [];
    for (const p of frontier) {
      if (cover === 'states' && p.n !== 0) want.add(String(p.n));
      for (const e of edges[p.n]) {
        const q = next(p, e);
        if (!q) continue;
        if (cover === 'transitions') want.add(key(p, e));
        if (!seen.has(pk(q))) { seen.add(pk(q)); more.push(q); }
      }
    }
    frontier = more;
  }
  interface Hop { from: P; e: Edge }
  const nearest = (cur: P): Hop[] => {
    const parent = new Map<string, Hop>();
    const seen = new Set<string>([pk(cur)]);
    const chain = (p: P) => {
      const hs: Hop[] = [];
      while (pk(p) !== pk(cur)) { const h = parent.get(pk(p))!; hs.unshift(h); p = h.from; }
      return hs;
    };
    for (let frontier: P[] = [cur]; frontier.length;) {
      const more: P[] = [];
      for (const p of frontier) {
        if (cover === 'states' && pk(p) !== pk(cur) && want.has(String(p.n))) return chain(p);
        for (const e of edges[p.n]) {
          const q = next(p, e);
          if (!q) continue;
          if (cover === 'transitions' && want.has(key(p, e))) return [...chain(p), { from: p, e }];
          if (!seen.has(pk(q))) { seen.add(pk(q)); parent.set(pk(q), { from: p, e }); more.push(q); }
        }
      }
      frontier = more;
    }
    return [];
  };
  const paths: Step[][] = [];
  for (;;) {
    let cur: P = { n: 0, landing: false };
    const trace: Step[] = [{ action: 'Init', state: g.nodes[0].state }];
    while (trace.length <= 50) {
      const hs = nearest(cur);
      if (!hs.length) break;
      for (const h of hs) {
        want.delete(key(h.from, h.e));
        want.delete(String(h.e.dest));
        trace.push({ action: `${ROLE}.${h.e.name}`, state: g.nodes[h.e.dest].state });
      }
      const last = hs[hs.length - 1];
      cur = next(last.from, last.e)!;
    }
    if (trace.length === 1) break;
    paths.push(trace);
  }
  if (want.size) throw new Error(`${SPEC}: steerable walks left ${want.size} targets uncovered`);
  return paths;
}

// ---- the record ----

interface Shadow {
  child: string; turn: string; job: string; typed: string; last: string;
  until: boolean; waits: boolean;
  pending: string[]; queued: string[]; model: string[]; lost: string[]; quit_log: string[];
  wakes: number; prompts: number; stops: number; budget: number;
}

const newShadow = (): Shadow => ({
  child: 'alive', turn: 'none', job: 'none', typed: 'none', last: '', until: false, waits: false,
  pending: [], queued: [], model: [], lost: [], quit_log: [], wakes: 0, prompts: 0, stops: 0, budget: 0,
});

const open = (s: Shadow) => s.turn === 'user' || s.turn === 'wake';
// A request boundary: the queued notices go to the model.
const flush = (s: Shadow) => { s.model.push(...s.queued); s.queued = []; };
const finish = (s: Shadow, job: string) => { s.job = job; s.typed = 'finished'; s.pending.push('finish'); };
const matchSeen = (s: Shadow) => [s.pending, s.queued, s.model, s.lost, s.quit_log].some((l) => l.includes('match'));

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  dir: string;       // llm-control's queue
  jobDir: string;    // the files that steer the job
  sh: Shadow;
  want: State;       // the step's state, for BashBackground's until
  seq: number;
  held: string;      // the llm-control turn in flight, '' when none
  takes: number;     // requests the open turn has made (the step budget's count)
  names: string[];   // every turn this session took, in order
  deferred: string[]; // job events the spec has made and the walk has not
  landing: boolean;  // a JobKill's notice is on its way
  limited: boolean;  // LimitExpires was made real as a kill
  ahead: '' | 'stop' | 'budget'; // the server is past the spec's step
  // Stopped before its first prompt: serve forgets a session that wrote
  // nothing, with its child, so its row and its reads are gone (404).
  forgotten: boolean;
}

// ---- the server, for waits only ----

interface Entry { kind: string; text: string; data: Record<string, unknown> }
interface Row { live?: boolean; status?: string }

// The history file: serve's transcript leaves out the typed job entries.
function history(c: Ctx): Entry[] {
  const f = path.join(c.serve.home, '.bough', 'history', `${c.id}.jsonl`);
  if (!fs.existsSync(f)) return [];
  return fs.readFileSync(f, 'utf8').split('\n').filter(Boolean).map((l) => {
    const e = JSON.parse(l);
    const data = { ...(e.data ?? {}) };
    const text = typeof data.text === 'string' ? data.text : '';
    delete data.text;
    return { kind: e.kind, text, data };
  });
}

async function row(c: Ctx): Promise<Row> {
  const r = await c.serve.api.get(`/api/sessions/${c.id}`);
  // A session stopped before it wrote anything is forgotten with its child.
  if (r.status() === 404) return {};
  if (!r.ok()) throw new Error(`GET session: ${r.status()}`);
  return (await r.json()).session ?? {};
}

async function wait(c: Ctx, what: string, ok: (r: Row, es: Entry[]) => boolean): Promise<void> {
  const deadline = Date.now() + BOUND;
  for (;;) {
    const r = await row(c);
    const es = history(c);
    if (ok(r, es)) return;
    if (Date.now() > deadline) throw new Error(`waiting for ${what}: row ${JSON.stringify(r)}, history ${es.map((e) => e.kind + (e.data.event ? ':' + e.data.event : '')).join(' ')}`);
    await new Promise((res) => setTimeout(res, 25));
  }
}

const isNote = (e: Entry) => e.kind === 'job' && e.data.event === undefined;
const isNoticeWake = (e: Entry) => e.kind === 'input' && e.data.wake === true && e.data.reason === 'notice';
const count = (es: Entry[], ok: (e: Entry) => boolean) => es.filter(ok).length;
const lastNote = (es: Entry[]) => [...es].reverse().find(isNote)?.text ?? '';

function lastCloseCancelled(es: Entry[]): boolean {
  let isOpen = false, last = '';
  for (const e of es) {
    if (e.kind === 'input') { isOpen = true; last = ''; }
    else if (e.kind === 'done' || e.kind === 'cancelled') {
      if (e.kind === 'done' && !isOpen && last === 'cancelled') continue;
      isOpen = false; last = e.kind;
    }
  }
  return !isOpen && last === 'cancelled';
}

// ---- the model ----

function nextName(c: Ctx): string {
  return `t${String(++c.seq).padStart(6, '0')}`;
}

function block(c: Ctx): string {
  const n = nextName(c);
  queue(c.dir, n, { mode: 'block', text: 'reply' });
  return n;
}

async function taken(c: Ctx, name: string): Promise<void> {
  await waitTaken(c.dir, name, BOUND);
  c.names.push(name);
  c.takes++;
}

// Release the held request as turn says and wait for the next request,
// which takes a fresh held turn.
async function answer(c: Ctx, turn: Turn | Record<string, unknown>): Promise<void> {
  const n = block(c);
  releaseWith(c.dir, c.held, turn as Turn);
  c.held = n;
  await taken(c, n);
}

// Every user message the session's requests carried.
function requests(c: Ctx): string[] {
  const all: string[] = [];
  for (const n of c.names) {
    const f = path.join(c.dir, `${n}.request`);
    if (fs.existsSync(f)) all.push(...JSON.parse(fs.readFileSync(f, 'utf8')));
  }
  return all;
}

// The job: a loop steered through files in jobDir (`match` prints the
// until pattern once, `exit` exits 0). The perl helper leaves the job's
// process group, so a kill leaves it alive, and holds the job's stdout
// until the shell is gone, a second longer when `slow` exists: the job's
// notice lands only when the pipe closes. The loop ends with its child.
function script(c: Ctx): string {
  return `D=${JSON.stringify(c.jobDir)}; P=$PPID
perl -e 'setpgrp(0,0); my $p = getppid(); while (kill(0, $p)) { select(undef, undef, undef, 0.02) } select(undef, undef, undef, 1.0) if -e "$ARGV[0]/slow"' "$D" &
while :; do
  if [ -e "$D/match" ] && [ ! -e "$D/matched" ]; then : > "$D/matched"; echo ${MATCH}; fi
  [ -e "$D/exit" ] && exit 0
  kill -0 "$P" 2>/dev/null || exit 3
  sleep 0.02
done`;
}

const touch = (c: Ctx, f: string) => fs.writeFileSync(path.join(c.jobDir, f), '');

// ---- the page ----

const sidebarRow = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const workDialog = (c: Ctx) => c.page.getByRole('dialog', { name: /^Work/ });

// The Work dialog's Stop on job 1: serve sends the child "/jobkill 1".
async function killFromWork(c: Ctx): Promise<void> {
  await c.page.locator('button.work-summary').click();
  const res = c.page.waitForResponse((r) => r.url().includes(`/api/sessions/${c.id}/jobs/1/kill`));
  await workDialog(c).getByRole('button', { name: 'Stop Job 1', exact: true }).click();
  const r = await res;
  if (!r.ok()) throw new Error(`kill job 1: ${r.status()}`);
  await c.page.keyboard.press('Escape');
  await expect(workDialog(c)).toBeHidden();
}

async function trigger(c: Ctx, ev: string): Promise<void> {
  if (ev === 'match') touch(c, 'match');
  else if (ev === 'exit') touch(c, 'exit');
  else await killFromWork(c); // kill, and limit made real as a kill
}

// Make the deferred job events real and wait for their one notice: a
// wake turn when none is open (its request takes a held turn), a note
// queued in the turn in flight otherwise.
async function deliver(c: Ctx): Promise<void> {
  const es = history(c);
  const wakes = count(es, isNoticeWake), notes = count(es, isNote);
  const idle = c.held === '';
  const w = idle ? block(c) : '';
  for (const ev of c.deferred) await trigger(c, ev);
  c.deferred = [];
  if (idle) {
    await wait(c, 'the notice to open a wake turn', (_, l) => count(l, isNoticeWake) > wakes);
    c.held = w;
    c.takes = 0;
    await taken(c, w);
    return;
  }
  await wait(c, 'the notice to be queued in the open turn', (_, l) => count(l, isNote) > notes);
}

// ---- readUiState ----

interface Section { kind: 'user' | 'wake' | 'none'; wake: string[]; notes: string[]; closed: boolean; stopped: boolean }
interface Dom { word: string; live: boolean; work: string | null; job: { state: string; cause: string } | null; sections: Section[] }

async function readDom(c: Ctx): Promise<Dom> {
  return c.page.evaluate((id) => {
    const txt = (e: Element | null | undefined) => (e?.textContent ?? '').replace(/\s+/g, ' ').trim();
    const row = document.querySelector(`button.row[data-id="${id}"]`);
    const label = row?.getAttribute('aria-label') ?? '';
    // A notice as the page shows it: the job's row for a finish, a
    // "matched" line for a match.
    const item = (s: Element): string | null => {
      if (s.classList.contains('job-summary')) return 'finish';
      if (txt(s.querySelector('.block-label')) === 'Job' && /^\d+ matched /.test(txt(s.querySelector('.block-detail')))) return 'match';
      return null;
    };
    const sections = [...document.querySelectorAll('.transcript section.turn')].map((s) => {
      const banner = s.querySelector('.job-wake');
      const wake: string[] = [];
      if (banner) {
        for (const j of banner.querySelectorAll('summary.job-summary')) wake.push(item(j)!);
        if (banner.classList.contains('call-wake')) wake.push(/matched/.test(txt(banner)) ? 'match' : `unknown wake: ${txt(banner)}`);
      }
      const notes: string[] = [];
      for (const sum of s.querySelectorAll('summary')) {
        if (banner?.contains(sum)) continue;
        const it = item(sum);
        if (it) notes.push(it);
      }
      const out = s.querySelector('.turn-outcome');
      return {
        kind: (s.querySelector('.prompt') ? 'user' : banner ? 'wake' : 'none') as 'user' | 'wake' | 'none',
        wake, notes, closed: !!out, stopped: !!out?.classList.contains('turn-stopped'),
      };
    });
    const js = document.querySelector('.transcript summary.job-summary');
    return {
      word: label.split(', ')[1] ?? '',
      live: !!row?.hasAttribute('data-live'),
      work: document.querySelector('button.work-summary')?.getAttribute('aria-label') ?? null,
      // "Job 1, Failed, 2 seconds: cmd" or "Job 1, Finished: cmd".
      job: js ? { state: /^Job \d+, ([^,:]*)/.exec(js.getAttribute('aria-label') ?? '')?.[1] ?? '', cause: txt(js.querySelector('.job-cause')) } : null,
      sections,
    };
  }, c.id);
}

// Job 1 as the page shows it: running while the Work button says so,
// then its row's state word, and for a job that did not exit on its own
// the reason it was killed.
function jobOf(d: Dom): { job: string; typed: string } {
  if (d.work === null) return { job: 'none', typed: 'none' };
  if (/\brunning\b/.test(d.work)) return { job: 'running', typed: 'started' };
  if (!d.job) return { job: `finished, no job row (${d.work})`, typed: 'finished' };
  if (d.job.state === 'Finished') return { job: 'exited', typed: 'finished' };
  const cause = d.job.cause.toLowerCase();
  const job = /killed when bough quit/.test(cause) ? 'quit' : /killed after/.test(cause) ? 'limit' : /^killed/.test(cause) ? 'killed' : `${d.job.state}: ${d.job.cause}`;
  return { job, typed: 'finished' };
}

async function readUiState(c: Ctx): Promise<State> {
  const sh = c.sh;
  const st: State = {
    child: sh.child, turn: sh.turn, job: sh.job, until: sh.until, typed: sh.typed,
    pending: [...sh.pending], queued: [...sh.queued], waits: sh.waits, model: [...sh.model],
    lost: [...sh.lost], quit_log: [...sh.quit_log], wakes: sh.wakes, last: sh.last,
    prompts: sh.prompts, stops: sh.stops, budget: sh.budget,
  };
  // Stop ran the child's whole exit at once: until ChildExits the page is
  // past the spec's steps, which are the record's.
  if (c.ahead === 'stop') return st;
  const d = await readDom(c);
  const reqs = requests(c);
  const sent = (it: string) => reqs.some((m) => m.startsWith('[notice]') && (it === 'match' ? / matched "/.test(m) : /\bjob \d+ \[/.test(m)));
  const r = { queued: [] as string[], model: [] as string[], lost: [] as string[], quit_log: [] as string[] };
  let wakes = 0, last = '';
  for (const s of d.sections) {
    if (s.kind === 'none') { r.quit_log.push(...s.notes); continue; }
    if (s.kind === 'wake') { wakes++; r.model.push(...s.wake); }
    for (const n of s.notes) (sent(n) ? r.model : s.closed ? r.lost : r.queued).push(n);
    if (s.closed) last = s.stopped ? 'cancelled' : s.kind;
  }
  const lastOpen = d.sections.filter((s) => s.kind !== 'none').at(-1);
  if (c.ahead === '') {
    st.child = d.live ? 'alive' : 'gone';
    st.turn = d.word === 'Running' ? (lastOpen?.kind === 'wake' ? 'wake' : 'user') : 'none';
    st.last = last;
  }
  if (c.deferred.length === 0 && !c.landing) {
    const j = jobOf(d);
    st.typed = j.typed;
    if (!c.limited) st.job = j.job;
  }
  st.wakes = wakes;
  Object.assign(st, r);
  return st;
}

// ---- the actions ----

type Action = (c: Ctx) => Promise<void>;

const actions: Record<string, Action> = {
  async Prompt(c) {
    const n = block(c);
    await c.page.locator('#composer').fill(`turn ${n}`);
    await c.page.getByRole('button', { name: 'Send', exact: true }).click();
    c.held = n;
    c.takes = 0;
    await taken(c, n);
    c.sh.prompts++;
    c.sh.turn = 'user';
  },

  async Stop(c) {
    const sh = c.sh;
    const isOpen = open(sh), running = sh.job === 'running';
    if (isOpen) {
      await c.page.getByRole('button', { name: 'Stop', exact: true }).click();
    } else {
      // No turn is running, so the page offers no Stop: serve's
      // /interrupt, as another client would send it.
      const res = await c.serve.api.post(`/api/sessions/${c.id}/interrupt`);
      if (!res.ok()) throw new Error(`interrupt: ${res.status()} ${await res.text()}`);
    }
    await wait(c, 'the stop to end the child', (r, es) => {
      if (r.live || (isOpen && !lastCloseCancelled(es))) return false;
      return !running || lastNote(es).includes('killed when bough quit');
    });
    c.held = '';
    c.ahead = 'stop';
    c.forgotten = !fs.existsSync(path.join(c.serve.home, '.bough', 'history', `${c.id}.jsonl`));
    sh.stops++;
    sh.child = 'exiting';
    if (isOpen) {
      sh.lost.push(...sh.queued);
      sh.queued = [];
      sh.turn = 'cancelling';
    }
  },

  async KillJob(c) {
    c.deferred.push('kill');
    finish(c.sh, 'killed');
  },

  async BashBackground(c) {
    const until = c.want[`${ROLE}.until`] === true;
    const args: Record<string, unknown> = { command: script(c), background: true, timeout: '300s' };
    if (until) args.until = MATCH;
    await answer(c, { mode: 'call', tool: 'bash', args });
    await wait(c, "the job's started entry", (_, es) => es.some((e) => e.kind === 'job' && e.data.event === 'started'));
    flush(c.sh);
    Object.assign(c.sh, { job: 'running', typed: 'started', until });
  },

  async JobKill(c) {
    touch(c, 'slow');
    await answer(c, { mode: 'call', tool: 'job_kill', args: { id: 1 } });
    c.landing = true;
    flush(c.sh);
    finish(c.sh, 'killed');
  },

  // A job call with the job still running waits for a change, so while
  // it runs the model makes another call (the spec's Poll is only its
  // boundary).
  async Poll(c) {
    const running = c.sh.job === 'running' || c.deferred.length > 0;
    await answer(c, running
      ? { mode: 'call', tool: 'bash', args: { command: 'true' } }
      : { mode: 'call', tool: 'job', args: { id: 1 } });
    flush(c.sh);
  },

  async Reply(c) {
    const sh = c.sh;
    if (sh.queued.length > 0) {
      // The notices sent at this boundary keep the turn open.
      await answer(c, { mode: 'ok', text: 'reply' });
      flush(sh);
      return;
    }
    release(c.dir, c.held);
    c.held = '';
    await wait(c, 'the reply to close the turn', (r, es) => r.status !== 'running' && !turnState(es).open);
    sh.last = sh.turn;
    sh.turn = 'none';
  },

  async OutputMatchesUntil(c) {
    c.deferred.push('match');
    c.sh.pending.push('match');
  },

  async JobExits(c) {
    c.deferred.push('exit');
    finish(c.sh, 'exited');
  },

  // The limit's timer fires at a time, not on cue: made real as a kill
  // (the same Wait and notice path), so job is the record from here on.
  async LimitExpires(c) {
    c.deferred.push('limit');
    c.limited = true;
    finish(c.sh, 'limit');
  },

  // The held request answers with a call, and padding calls follow until
  // the gate refuses the next request; the cancel closes with it.
  async BudgetStop(c) {
    const pads = MAX_STEPS - c.takes;
    if (pads < 0) throw new Error(`budget stop: the turn already made ${c.takes} requests`);
    const names: string[] = [];
    for (let i = 0; i < pads; i++) {
      const n = nextName(c);
      queue(c.dir, n, { mode: 'ok', calls: [{ name: 'job_kill', args: { id: 1000 + i } }] } as unknown as Turn);
      names.push(n);
    }
    releaseWith(c.dir, c.held, { mode: 'call', tool: 'job_kill', args: { id: 999 } });
    c.held = '';
    for (const n of names) await taken(c, n);
    await wait(c, 'the step budget to stop the turn', (_, es) => { const t = turnState(es); return !t.open && t.last === 'cancelled'; });
    c.ahead = 'budget';
    const sh = c.sh;
    sh.budget++;
    sh.lost.push(...sh.queued);
    sh.queued = [];
    sh.turn = 'cancelling';
  },

  async NoticeDelivered(c) {
    const sh = c.sh;
    if (c.landing) {
      c.landing = false;
      await wait(c, "the killed job's notice to be queued", (_, es) => lastNote(es).includes('job 1 ['));
    } else if (sh.turn !== 'cancelling') {
      await deliver(c);
    }
    // Held behind a cancel: the real cancel has closed (ahead), and the
    // events stay deferred until CancelCloses takes them.
    if (sh.turn === 'cancelling') sh.waits = true;
    else if (sh.turn === 'none') {
      sh.model.push(...sh.pending);
      sh.pending = [];
      sh.wakes++;
      sh.turn = 'wake';
    } else {
      sh.queued.push(...sh.pending);
      sh.pending = [];
    }
  },

  async CancelCloses(c) {
    const sh = c.sh;
    sh.turn = 'none';
    sh.last = 'cancelled';
    let wake = false;
    if (sh.waits) {
      sh.waits = false;
      if (sh.pending.length > 0) {
        wake = true;
        sh.model.push(...sh.pending);
        sh.pending = [];
        sh.wakes++;
        sh.turn = 'wake';
      }
    }
    if (c.ahead === 'budget') {
      c.ahead = '';
      if (wake) await deliver(c);
    }
  },

  async ChildExits(c) {
    const sh = c.sh;
    c.ahead = ''; // the server did this at Stop; its state is the spec's again
    if (sh.turn === 'wake') { sh.turn = 'none'; sh.last = 'cancelled'; }
    if (sh.job === 'running') finish(sh, 'quit');
    sh.lost.push(...sh.queued);
    sh.queued = [];
    sh.quit_log.push(...sh.pending);
    sh.pending = [];
    sh.waits = false;
    sh.child = 'gone';
  },
};

// The history's turn state: whether a turn is open, and how the last one
// closed ("done", or "cancelled" for a cancel or a budget stop's done,
// which carries stop). The done after a cancel is the same close.
function turnState(es: Entry[]): { open: boolean; last: string } {
  let isOpen = false, last = '';
  for (const e of es) {
    if (e.kind === 'input' && e.data.steer === undefined) isOpen = true;
    else if ((e.kind === 'done' || e.kind === 'cancelled') && isOpen) {
      isOpen = false;
      last = e.kind === 'cancelled' || e.data.stop !== undefined ? 'cancelled' : 'done';
    }
  }
  return { open: isOpen, last };
}

// ---- the walker ----

// The shape of helpers/model.ts's modelTests, over the steerable walks:
// at every node the page's state must equal the spec's, the page must not
// scroll sideways, the row must be on screen and nothing logged an error.
const g = loadGraph(path.resolve(__dirname, '..', '..', '..', 'model', 'testdata', SPEC));
const cover = process.env.MODEL_COVER === 'transitions' ? 'transitions' : 'states';

test.use({ serveOpts: { config: CONFIG } });

test.describe(`model: ${SPEC}`, () => {
  steerable(g, cover).forEach((trace, i) => {
    const name = (a: string) => (a === 'Init' ? a : a.slice(ROLE.length + 1));
    const title = `path ${i}: ${trace.slice(1).map((s) => name(s.action)).join(' → ')}`;
    test(title, async ({ serve, page }, info) => {
      test.setTimeout(30_000 + trace.length * 15_000);
      const errors: string[] = [];
      let c: Ctx | undefined;
      page.on('console', (m) => {
        if (m.type() !== 'error') return;
        // The open page's reads of a session serve forgot.
        if (c?.forgotten && /status of 404/.test(m.text())) return;
        errors.push(m.text());
      });
      page.on('pageerror', (e) => errors.push(String(e)));
      await page.clock.install();
      const id = await serve.newSession();
      const jobDir = path.join(serve.home, 'job');
      fs.mkdirSync(jobDir, { recursive: true });
      c = {
        page, serve, id, dir: controlDir(serve.home), jobDir, sh: newShadow(), want: {}, seq: 0, held: '', takes: 0,
        names: [], deferred: [], landing: false, limited: false, ahead: '', forgotten: false,
      };
      await page.goto(`${serve.url}/#/s/${id}`);
      try {
        for (const [n, step] of trace.entries()) {
          const act = name(step.action);
          const where = `step ${n} (${act})`;
          c.want = step.state;
          // "end" is fizz's self-link on a state with nothing enabled: nothing happens.
          if (n > 0 && act !== 'end') {
            const perform = actions[act];
            if (!perform) throw new Error(`${SPEC}: no action for ${step.action}`);
            await perform(c);
          }
          await expect.poll(async () => {
            await page.clock.fastForward(5_000);
            return readUiState(c);
          }, { message: `${where}: state`, timeout: 10_000 }).toEqual(roleState(ROLE, step.state));
          const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
          expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
          if (c.forgotten) await expect(sidebarRow(c), `${where}: a forgotten session keeps its row`).toHaveCount(0);
          else await expect(sidebarRow(c), `${where}: status not visible`).toBeVisible();
          expect(errors, `${where}: console errors`).toEqual([]);
        }
      } finally {
        if (c.held) release(c.dir, c.held);
        saveTranscript(serve, id, info);
      }
    });
  });
});

// The transcript, for TestHistoryTraces in go/tests/model/mbt.
function saveTranscript(serve: Serve, id: string, info: TestInfo): void {
  const root = process.env.MODEL_TRACE_DIR;
  if (!root) return;
  const src = path.join(serve.home, '.bough', 'history', `${id}.jsonl`);
  if (!fs.existsSync(src)) return;
  const dir = path.join(root, SPEC);
  fs.mkdirSync(dir, { recursive: true });
  fs.copyFileSync(src, path.join(dir, `${info.title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '')}-${id}.jsonl`));
}
