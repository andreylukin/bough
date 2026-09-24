// go/tests/model/specs/ui_composer.fizz in the browser: the web composer
// as a person drives it (draft, send, steer, queue, attach, the / @ and
// Skills pickers, the model and effort Selects, failure rows, archive,
// and a second session's draft). Every generated path is walked with
// real clicks and keys against the worker's serve (llm-control as the
// model), and at every node the state read off the DOM must be the
// spec's, and the screen must owe nothing: no sideways scroll, no
// clipped status or header text, no console error, a visible focus ring
// on whatever holds focus, and axe finds nothing on the composer.
// Recipe: go/tests/model/README.md.
//
// It walks with its own loop, not helpers/model.ts's modelTests, because
// the server's and the page's steps have to be held for a node to be
// looked at:
//
// - A prompt the server took is not recorded until Land: a
//   user-prompt-submit hook in serve's HOME waits for a gate file named
//   by a hash of the prompt's text. A steer's gate is opened as it is
//   sent. (Holding the POST instead would keep the page "busy", and a
//   busy page refuses the steer the spec allows while a send is out.)
// - A send the spec fails (SendFails) is held at the route and answered
//   500; so are the list reads the spec fails (ListFails, the skills and
//   files the pickers read) and an upload (UploadFails). A list read or
//   an upload the spec lets through is held until ListLoads / UploadDone.
// - Flush is the page's own step (the queue's head goes the moment the
//   turn ends). A read may match a later node on the path when every
//   step in between is a Flush; where the path's next step is not the
//   Flush the page already took, the walk ends there as "not taken"
//   (the Go adapter's fmbt.ErrNotImplemented), and the report counts
//   only nodes the page was seen in.
//
// UI_COMPOSER_COVERAGE=<dir> makes each walk append the states and
// transitions it saw there (one JSONL file per worker), for the
// walked-vs-total count.
//
// draft_b while A is on screen (and draft_a while B is) is not on the
// screen; it is read from the page's own storage (bough:draft:<id>), the
// same place the page restores it from.
import AxeBuilder from '@axe-core/playwright';
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route, TestInfo } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, releaseWith, waitTaken } from '../../helpers/control';
import { loadPaths, roleState, type Step } from '../../helpers/model';
import { test, expect, type Serve } from '../../helpers/serve';

const SPEC = 'ui_composer';
const ROLE = 'Composer#0';
type State = Record<string, unknown>;

// The prompt hook: the input waits for $HOME/gates/<hash of its text>.
// Bounded, so a walk that never lands a prompt leaves no child behind.
const GATE_HOOK = [
  'const t = String(event.input).trim();',
  'let h = 5381;',
  'for (let i = 0; i < t.length; i++) h = ((h * 33) ^ t.charCodeAt(i)) >>> 0;',
  "tools.bash('for i in $(seq 1 1200); do [ -e \"$HOME/gates/' + h.toString(16) + '\" ] && break; sleep 0.05; done');",
  'return null;',
].join('\n');

function gateKey(text: string): string {
  const t = text.trim();
  let h = 5381;
  for (let i = 0; i < t.length; i++) h = ((h * 33) ^ t.charCodeAt(i)) >>> 0;
  return h.toString(16);
}

test.use({
  workerServeOpts: {
    config: CONTROL_CONFIG,
    home: {
      '.bough/hooks/user-prompt-submit/gate.js': GATE_HOOK,
      // Something for the @ picker to list.
      'work/notes.md': '# notes\n',
    },
  },
});

// ---- the graph, from the paths (together they cover every transition) ----

const bare = (action: string) => (action.startsWith(ROLE + '.') ? action.slice(ROLE.length + 1) : action);
const key = (s: State) => JSON.stringify(Object.keys(s).sort().map((k) => [k, s[k]]));
const AUTOMATIC = new Set(['Flush']);

const paths: Step[][] = loadPaths(SPEC);
const graph = new Map<string, { action: string; to: State }[]>();
const allNodes = new Set<string>();
const allEdges = new Set<string>();
for (const trace of paths) {
  allNodes.add(key(roleState(ROLE, trace[0].state)));
  for (let i = 1; i < trace.length; i++) {
    const from = roleState(ROLE, trace[i - 1].state), to = roleState(ROLE, trace[i].state), action = bare(trace[i].action);
    allNodes.add(key(to));
    allEdges.add(`${key(from)} ${action} ${key(to)}`);
    const out = graph.get(key(from)) ?? [];
    if (!out.some((e) => e.action === action && key(e.to) === key(to))) out.push({ action, to });
    graph.set(key(from), out);
  }
}
const hasAuto = (s: State) => (graph.get(key(s)) ?? []).some((e) => AUTOMATIC.has(e.action));

// Every state the page may show once `s` is on screen: s, and whatever
// the page's own steps make of it.
function autoClosure(s: State): Set<string> {
  const out = new Set<string>();
  const todo = [s];
  while (todo.length) {
    const x = todo.pop()!;
    if (out.has(key(x))) continue;
    out.add(key(x));
    for (const e of graph.get(key(x)) ?? []) if (AUTOMATIC.has(e.action)) todo.push(e.to);
  }
  return out;
}

// ---- the walk's hands ----

interface Ctx {
  page: Page;
  serve: Serve;
  dir: string;       // llm-control's queue
  gates: string;     // the prompt hook's gate files
  a: string;
  b: string;
  tag: string;       // unique per walk, in every word it types
  words: number;
  held: string;      // the model turn A's running turn holds, '' when none
  expect: ('prompt' | 'steer' | 'fail')[]; // what the next POST /prompt is, oldest first
  failing: Route[];  // POST /prompt held to be answered 500 (SendFails)
  lists: Route[];    // GET /api/skills and /api/files held until ListLoads / ListFails
  upload: Route[];   // POST /api/attachments held until UploadDone / UploadFails
  steers: Set<string>; // texts sent as steers: their rows are not "pending"
  gated: string[];   // gate keys of prompts sent and not landed
  allow: RegExp | null; // the console error the step in flight owes
}

// Model turn names are taken in lexical order and the worker's serve
// outlives a walk: the counter is per worker (per process).
let turns = 0;
const turnName = () => `u${String(++turns).padStart(8, '0')}`;

const composer = (c: Ctx) => c.page.locator('#composer');
const rowOf = (c: Ctx, id: string) => c.page.locator(`button.row[data-id="${id}"]`).first();
const word = (c: Ctx) => `${c.tag}w${++c.words}`;

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`${SPEC}: waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

async function apiStatus(c: Ctx): Promise<string> {
  const res = await c.serve.api.get(`/api/sessions/${c.a}`);
  return (await res.json()).session.status;
}

function openGate(c: Ctx, k: string): void {
  fs.writeFileSync(path.join(c.gates, k), '');
}

// Where the keys go: Enter in the composer when it has focus, else the
// primary button, as a person with the pointer does.
async function focused(c: Ctx): Promise<boolean> {
  return c.page.evaluate(() => document.activeElement?.id === 'composer');
}

const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64');

type Act = (c: Ctx, before: State, after: State, trace: Step[], n: number) => Promise<void>;

// The step that settles a prompt sent at step n: Land, SendFails, or
// nothing on this path.
function resolution(trace: Step[], n: number): string {
  for (let m = n + 1; m < trace.length; m++) {
    if (!roleState(ROLE, trace[m].state).pending) return bare(trace[m].action);
  }
  return 'END';
}

// The next POST /prompt: a steer when A is live, else a prompt, held to
// fail when this path fails it.
function expectSend(c: Ctx, before: State, trace: Step[], n: number): void {
  const live = before.session === 'a' && (before.status === 'running' || before.pending);
  c.expect.push(live ? 'steer' : resolution(trace, n) === 'SendFails' ? 'fail' : 'prompt');
}

const actions: Record<string, Act> = {
  async Type(c, before) {
    await composer(c).press('End');
    await c.page.keyboard.type(before.popover === 'none' ? word(c) : ` ${word(c)}`);
  },
  async Newline(c) {
    await composer(c).press('End');
    await composer(c).press('Shift+Enter');
    await c.page.keyboard.type(word(c));
  },
  async TypeTrigger(c, before, after) {
    await composer(c).press('End');
    const sigil = after.popover === 'slash' ? '/' : '@';
    await c.page.keyboard.type(before.draft_a === '' ? sigil : ` ${sigil}`);
  },
  async ListLoads(c) {
    await until('the list read', () => c.lists.length > 0);
    for (const r of c.lists.splice(0)) await r.continue();
  },
  async ListFails(c) {
    c.allow = /status of 500/;
    await until('the list read', () => c.lists.length > 0);
    for (const r of c.lists.splice(0)) await r.fulfill({ status: 500, body: 'list read failed' });
  },
  async RetryList(c, before) {
    const scope = before.popover === 'skills' ? c.page.locator('.skills-pop') : c.page.locator('.mention');
    await scope.getByRole('button', { name: 'Retry' }).click();
  },
  // Enter picks the highlighted row, in the caret picker and the Skills filter alike.
  async PickItem(c) {
    await c.page.keyboard.press('Enter');
  },
  async Escape(c) {
    await c.page.keyboard.press('Escape');
  },
  // A click on the Skills button. Open, its scrim lies over the button, so
  // the second click lands on the scrim, which closes it the same way.
  async ClickSkills(c) {
    const box = await c.page.getByRole('button', { name: 'Skills', exact: true }).boundingBox();
    if (!box) throw new Error('no Skills button');
    await c.page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  },
  async ClickSetting(c, before, after) {
    const which = before.popover !== 'none' ? before.popover : after.popover;
    await c.page.locator(`.composer-tools button[aria-label^="Next turn ${which}"]`).click();
  },
  // The next option down from the current one, and Enter.
  async PickSetting(c, before) {
    if (before.status === 'archived') c.allow = /status of 409/;
    await c.page.keyboard.press('ArrowDown');
    await c.page.keyboard.press('Enter');
  },
  async FocusComposer(c) {
    await c.page.keyboard.press('Alt+KeyI');
  },
  // A paste, as the clipboard hands it to the page: the synthetic event
  // carries what a real Cmd+V would (headless Chromium has no clipboard
  // with a file in it). Long text becomes a tag at once; an image uploads.
  async Paste(c, _before, after) {
    const image = after.draft_a === 'uploading';
    const text = Array.from({ length: 20 }, (_, i) => `${word(c)} line ${i}`).join('\n');
    await composer(c).evaluate((el, { text, image, png }) => {
      const dt = new DataTransfer();
      if (image) dt.items.add(new File([Uint8Array.from(atob(png), (ch) => ch.charCodeAt(0))], 'shot.png', { type: 'image/png' }));
      else dt.setData('text/plain', text);
      el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    }, { text, image, png: PNG.toString('base64') });
  },
  async UploadDone(c) {
    await until('the upload', () => c.upload.length > 0);
    for (const r of c.upload.splice(0)) await r.continue();
  },
  async UploadFails(c) {
    c.allow = /status of 500/;
    await until('the upload', () => c.upload.length > 0);
    for (const r of c.upload.splice(0)) await r.fulfill({ status: 500, body: 'upload failed' });
  },
  async ClearDraft(c) {
    await composer(c).press('ControlOrMeta+a');
    await composer(c).press('Backspace');
  },
  async Send(c, before, _after, trace, n) {
    expectSend(c, before, trace, n);
    if (await focused(c)) await composer(c).press('Enter');
    else await c.page.locator('.composer .btn-primary').click();
  },
  async Enqueue(c) {
    if (await focused(c)) await composer(c).press('ControlOrMeta+Enter');
    else await c.page.getByRole('button', { name: 'Queue', exact: true }).click();
  },
  async Flush() { /* the page's flush effect */ },
  // The prompt is recorded and its turn starts: a held model turn is
  // queued for it and its gate opened.
  async Land(c) {
    c.held = turnName();
    queue(c.dir, c.held, { mode: 'block', text: 'answered' });
    for (const k of c.gated.splice(0)) openGate(c, k);
    await waitTaken(c.dir, c.held);
    await until('A running', async () => (await apiStatus(c)) === 'running');
  },
  async SendFails(c) {
    c.allow = /status of 500/;
    await until('the send to fail', () => c.failing.length > 0);
    for (const r of c.failing.splice(0)) await r.fulfill({ status: 500, contentType: 'text/plain', body: 'send failed' });
  },
  async Finish(c) {
    const t = c.held;
    c.held = '';
    releaseWith(c.dir, t, { mode: 'ok', text: `finished ${t}` });
    await until('the turn to end', async () => (await apiStatus(c)) !== 'running');
  },
  // Esc in the composer, else the Stop button. A turn stopped before any
  // reply puts its prompt back; for the path where it does not, the
  // model first says something (and holds on), so there is a reply.
  async Stop(c, before, after) {
    if (before.draft_a === '' && after.draft_a === '' && c.held) {
      const next = turnName();
      queue(c.dir, next, { mode: 'block', text: 'answered' });
      fs.writeFileSync(path.join(c.dir, c.held + '.release-tmp'), JSON.stringify({ text: `reply ${next}`, call: { name: 'no_such_tool' } }));
      fs.renameSync(path.join(c.dir, c.held + '.release-tmp'), path.join(c.dir, c.held + '.release'));
      await waitTaken(c.dir, next);
      c.held = next;
      await expect(c.page.locator('.thread').getByText(`reply ${next}`)).toBeVisible();
    }
    if (await focused(c)) await composer(c).press('Escape');
    else await c.page.getByRole('button', { name: 'Stop', exact: true }).click();
    await until('the turn to stop', async () => (await apiStatus(c)) !== 'running');
    c.held = '';
  },
  async Retry(c, before, _after, trace, n) {
    expectSend(c, before, trace, n);
    await c.page.locator('.send-failed').getByRole('button', { name: 'Retry', exact: true }).click();
  },
  async EditFailed(c) {
    await c.page.locator('.send-failed').getByRole('button', { name: /^Edit/ }).click();
  },
  // Discard sits in the row's details: open them, then Discard.
  async Discard(c) {
    const row = c.page.locator('.send-failed').first();
    await row.locator('summary').click();
    await row.getByRole('button', { name: 'Discard', exact: true }).click();
  },
  async Archive(c) {
    await c.page.getByRole('button', { name: 'Session settings' }).click();
    await c.page.getByRole('dialog', { name: 'Session settings' }).getByRole('button', { name: /^Archive/ }).click();
    await c.page.getByRole('dialog').getByRole('button', { name: 'Archive', exact: true }).click();
  },
  async Unarchive(c) {
    await c.page.locator('.archived-note').getByRole('button', { name: 'Unarchive' }).click();
  },
  async Switch(c, before) {
    await rowOf(c, before.session === 'a' ? c.b : c.a).click();
  },
};

// ---- readUiState: the role's fields, from the page ----

const WAITING = 'Model is thinking';

async function read(c: Ctx): Promise<State> {
  const dom = await c.page.evaluate(({ a, b, steers }) => {
    const hash = location.hash;
    const session = hash === `#/s/${a}` ? 'a' : hash === `#/s/${b}` ? 'b' : `other: ${hash}`;
    const box = document.querySelector<HTMLTextAreaElement>('#composer');
    const txt = (sel: string) => [...document.querySelectorAll<HTMLElement>(sel)].map((e) => (e.textContent ?? '').trim());
    const norm = (s: string) => s.replace(/\s+/g, ' ').trim();
    const steer = new Set(steers.map(norm));
    const unlanded = [...document.querySelectorAll('.thread section.turn:not([data-turn]) .prompt-bubble p')].map((e) => norm(e.textContent ?? ''));
    const rowA = document.querySelector(`button.row[data-id="${a}"]`)?.getAttribute('aria-label') ?? '';
    const primary = document.querySelector<HTMLButtonElement>('.composer .btn-primary');
    const edit = document.querySelector<HTMLButtonElement>('.send-failed .composer-edit');
    const act = document.activeElement as HTMLElement | null;
    const openSel = document.querySelector('.composer-tools .sel.sel-open');
    const mention = document.querySelector('.mention');
    const skills = document.querySelector('.skills-pop');
    let popover = 'none', listing = 'list';
    if (skills) {
      popover = 'skills';
      const empty = skills.querySelector('.skills-empty')?.textContent ?? '';
      listing = skills.querySelector('[role=option]') ? 'list' : /Couldn/.test(empty) ? 'error' : 'loading';
    } else if (openSel) {
      popover = /^Next turn model/.test(openSel.querySelector('.sel-btn')?.getAttribute('aria-label') ?? '') ? 'model' : 'effort';
      listing = openSel.querySelector('.sel-save-failed') ? 'error' : 'list';
    } else if (mention) {
      const list = mention.querySelector('[role=listbox]');
      const state = mention.querySelector('.mention-state')?.textContent ?? '';
      popover = (list?.getAttribute('aria-label') ?? state).match(/skills/i) ? 'slash' : 'mention';
      listing = list?.querySelector('[role=option]') ? 'list' : /Couldn/.test(state) ? 'error' : 'loading';
    }
    return {
      session,
      value: box?.value ?? null,
      disabled: box?.disabled ?? true,
      archived: !!document.querySelector('.archived-note'),
      rowA,
      unlanded: unlanded.filter((t) => !steer.has(t)).length,
      queued: document.querySelectorAll('ol.queued li.queued-row').length,
      failed: txt('.send-failed strong').includes('Not sent'),
      uploading: txt('.attach-note').some((t) => t.includes('Attaching')),
      lostNote: txt('.attach-note.attach-err').some((t) => /not attached|unavailable/.test(t)),
      stored: { a: localStorage.getItem('bough:draft:' + a) ?? '', b: localStorage.getItem('bough:draft:' + b) ?? '' },
      popover, listing,
      focus: act?.id === 'composer' ? 'composer' : act && (act.closest('.skills-pop') || act.closest('.composer-tools .sel.sel-open')) ? 'popover' : 'elsewhere',
      placeholder: [...txt('.thread .prompt-bubble'), ...txt('ol.queued .queued-text')].some((t) => /\[(Image|File) #\d+\]/.test(t)),
      label: primary?.getAttribute('aria-label') ?? '',
      sendOn: primary ? !primary.disabled : false,
      queue: !!document.querySelector('.composer-queue'),
      stop: !!document.querySelector('.composer-stop, .composer-stop-retry'),
      edit: edit ? !edit.disabled : false,
      status: document.querySelector('.composer-status')?.textContent?.trim() ?? '',
      hint: document.querySelector('.composer-hint')?.textContent ?? null,
    };
  }, { a: c.a, b: c.b, steers: [...c.steers] });

  const classify = (v: string) => {
    if (!v.trim()) return '';
    if (dom.uploading) return 'uploading';
    const tag = /\[(Image|File) #\d+\]|\[Pasted text #\d+ \+\d+ lines\]/.test(v);
    if (tag && dom.lostNote) return 'lost';
    if (tag) return 'pasted';
    if (v.includes('\n')) return 'multi';
    return 'typed';
  };
  // Off screen, a draft is what the page stored for it (only ever plain here).
  const stored = (v: string) => (v.trim() ? (v.includes('\n') ? 'multi' : 'typed') : '');
  const shown = dom.value === null ? 'no composer' : classify(dom.value);
  const onA = dom.session === 'a';
  const word = dom.rowA.split(', ')[1] ?? '';
  const status = onA && dom.archived ? 'archived' : word === 'Running' ? 'running' : 'idle';
  const statusWord = !dom.status ? '' : dom.status.startsWith('Sending') ? 'sending'
    : dom.status.startsWith('Working') ? 'running'
    : dom.status.startsWith(WAITING) ? (dom.label === 'Steer' ? 'running' : 'sending') : `other: ${dom.status}`;
  const hint = dom.hint === null ? 'hidden' : /steer/.test(dom.hint) ? 'steer' : /stop/.test(dom.hint) ? 'live' : 'send';
  return {
    session: dom.session,
    status,
    pending: onA && dom.unlanded > 0,
    queued: onA ? dom.queued : 0,
    failed: onA && dom.failed,
    draft_a: onA ? shown : stored(dom.stored.a),
    draft_b: onA ? stored(dom.stored.b) : shown,
    popover: dom.popover,
    listing: dom.listing,
    focus: dom.focus,
    placeholder_sent: dom.placeholder,
    shown,
    composer_on: !dom.disabled,
    send_label: dom.label,
    send_enabled: dom.sendOn,
    queue_shown: dom.queue,
    stop_shown: dom.stop,
    edit_enabled: dom.edit,
    status_word: statusWord,
    hint,
  };
}

// ---- what every node owes ----

// Status and header text that is cut off says less than it should.
const CLIP = ['.composer-status', '.composer-hint .composer-key', '.thread-head h1', '.thread-head .head-main > .status',
  '.send-failed strong', '.archived-note .composer-note-lead', '.attach-note', '.sel-save', '.composer .btn-primary', '.sel-btn .sel-value'];

async function invariants(c: Ctx, errors: string[], where: string): Promise<void> {
  // Colours are judged at rest: a button fading in from disabled is
  // mid-transition for 150 ms, and axe read its contrast halfway.
  await c.page.evaluate(() => Promise.race([
    Promise.all(document.getAnimations().filter((a) => a.effect?.getComputedTiming().iterations !== Infinity).map((a) => a.finished.catch(() => {}))),
    new Promise((r) => setTimeout(r, 2000)),
  ]));
  const dom = await c.page.evaluate((clip) => {
    const overflow = document.documentElement.scrollWidth - document.documentElement.clientWidth;
    const clipped = clip.flatMap((sel) => [...document.querySelectorAll<HTMLElement>(sel)])
      .filter((e) => e.offsetParent && e.scrollWidth > e.clientWidth + 1)
      .map((e) => `${e.className}: "${e.textContent}" ${e.scrollWidth}>${e.clientWidth}`);
    // The focused control shows it: its own outline or shadow, or, for the
    // textarea, the composer box around it.
    const act = document.activeElement as HTMLElement | null;
    let ring = 'none';
    if (act && act !== document.body && act.matches(':focus-visible')) {
      const shows = (el: Element) => {
        const s = getComputedStyle(el);
        return (s.outlineStyle !== 'none' && parseFloat(s.outlineWidth) > 0) || s.boxShadow !== 'none';
      };
      ring = shows(act) || (act.id === 'composer' && act.parentElement && shows(act.parentElement)) ? 'ok'
        : `missing on ${act.tagName.toLowerCase()}.${act.className} "${act.getAttribute('aria-label') ?? act.textContent?.slice(0, 30)}"`;
    }
    return { overflow, clipped, ring };
  }, CLIP);
  expect(dom.overflow, `${where}: page scrolls sideways by ${dom.overflow}px`).toBeLessThanOrEqual(0);
  expect(dom.clipped, `${where}: clipped text`).toEqual([]);
  expect(dom.ring === 'none' || dom.ring === 'ok' ? 'ok' : dom.ring, `${where}: focus ring`).toBe('ok');
  await expect(composer(c), `${where}: composer not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
  const axe = await new AxeBuilder({ page: c.page }).include('.composer-wrap').withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa']).analyze();
  expect(axe.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`), `${where}: axe`).toEqual([]);
}

const SETTLE_MS = 10_000;
const STABLE_MS = 400;

// Waits for the page to show node n, or a later one reached by Flush
// alone. Returns the node matched, or -1 when the page settled on a state
// a Flush makes of node n that the path does not go on to.
async function settle(c: Ctx, trace: Step[], n: number, where: string, errors: string[]): Promise<number> {
  const want: string[] = [];
  for (let m = n; m < trace.length; m++) {
    if (m > n && !AUTOMATIC.has(bare(trace[m].action))) break;
    want.push(key(expected(c, roleState(ROLE, trace[m].state))));
  }
  const allowed = autoClosure(roleState(ROLE, trace[n].state));
  const deadline = Date.now() + SETTLE_MS;
  let last: State = {};
  let held = '', since = 0;
  for (;;) {
    last = await read(c);
    const k = key(last);
    if (k !== held) { held = k; since = Date.now(); }
    const i = want.indexOf(k);
    // A node the page leaves on its own (a Flush) must hold a moment
    // before it counts as seen, and so must a step that changes nothing:
    // the first read after the key is the state before it took effect.
    const selfLoop = n > 0 && key(roleState(ROLE, trace[n - 1].state)) === key(roleState(ROLE, trace[n].state));
    const brief = i >= 0 && (selfLoop || hasAuto(roleState(ROLE, trace[n + i].state)));
    if (i >= 0 && (!brief || Date.now() - since >= STABLE_MS)) return n + i;
    if (Date.now() > deadline) break;
    await new Promise((r) => setTimeout(r, 50));
  }
  if (allowed.has(key(last)) && !want.includes(key(last))) return -1;
  expect(last, `${where}: state${errors.length ? ` (console: ${errors.join(' | ')})` : ''}`).toEqual(expected(c, roleState(ROLE, trace[n].state)));
  return n;
}

// The spec's state as this harness can show it. A send the spec fails is
// held at the route until SendFails, so for those steps the request is
// out, and a page with a request out keeps Send disabled (busy: Enter is
// ignored too). A real failure answers in milliseconds; only the hold
// makes the gap one a person could type in. Everything else is the
// spec's, field for field.
function expected(c: Ctx, s: State): State {
  return c.failing.length && s.send_enabled ? { ...s, send_enabled: false } : s;
}

// ---- coverage: what the walks saw, written per worker for the report ----

function recordCoverage(info: { workerIndex: number; title: string }, seen: string[], edges: string[]): void {
  const dir = process.env.UI_COMPOSER_COVERAGE;
  if (!dir) return;
  fs.mkdirSync(dir, { recursive: true });
  fs.appendFileSync(path.join(dir, `w${info.workerIndex}.jsonl`), JSON.stringify({ title: info.title, seen, edges }) + '\n');
}

test.describe(`model: ${SPEC}`, () => {
  // fizz's 162 nodes and 496 transitions are what the generator covers;
  // told apart by the Composer's fields alone (all a page can show) they
  // are 120 states and 454 transitions, and those are what the walks are
  // counted against (UI_COMPOSER_COVERAGE).
  test('the walks cover the spec', () => {
    // Every settled state, always; every transition only on the
    // exhaustive run (MODEL_COVER=transitions), where the walks take
    // every link.
    expect(allNodes.size).toBe(120);
    if (process.env.MODEL_COVER === 'transitions') expect(allEdges.size).toBe(454);
  });

  paths.forEach((trace, i) => {
    const walk = trace.slice(1).map((s) => bare(s.action)).join(' → ');
    test(`path ${i}: ${walk}`, async ({ sharedServe: serve, page }, info) => {
      await walkPath(serve, page, info, trace, `p${i}r${info.retry}`);
    });
  });

  // The spec's self-loops are transitions the generator leaves out (they
  // change no state), and two of them are where the spec departs from the
  // code on purpose: with a tag whose file never arrived, Enter (a steer
  // here) and Cmd/Ctrl+Enter both leave the draft, the queue and the turn
  // as they were, and "[Image #1]" is never sent or queued as text.
  test('self-loops: a lost tag is neither steered nor queued', async ({ sharedServe: serve, page }, info) => {
    const base = paths.find((t) => t.some((s, k) => k > 0 && bare(s.action) === 'UploadFails'
      && roleState(ROLE, s.state).status === 'running'));
    if (!base) throw new Error('no path reaches a lost tag while A runs');
    const at = base.findIndex((s) => bare(s.action) === 'UploadFails');
    const lost = base[at].state;
    const trace = [...base.slice(0, at + 1), { action: `${ROLE}.Enqueue`, state: lost }, { action: `${ROLE}.Send`, state: lost }];
    await walkPath(serve, page, info, trace, `lr${info.retry}`);
  });
});

async function walkPath(serve: Serve, page: Page, info: TestInfo, trace: Step[], tag: string): Promise<void> {
  test.setTimeout(120_000);
  const c: Ctx = {
    page, serve, dir: controlDir(serve.home), gates: path.join(serve.home, 'gates'), a: '', b: '',
    tag, words: 0, held: '', expect: [], failing: [], lists: [], upload: [],
    steers: new Set(), gated: [], allow: null,
  };
  fs.mkdirSync(c.dir, { recursive: true });
  fs.mkdirSync(c.gates, { recursive: true });
  const errors: string[] = [];
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    if (c.allow?.test(m.text())) return;
    errors.push(m.text());
  });
  page.on('pageerror', (e) => errors.push(String(e.stack ?? e)));

  c.a = await serve.newSession();
  c.b = await serve.newSession();
  await page.route((u) => u.pathname === `/api/sessions/${c.a}/prompt`, async (route) => {
    const text = String(route.request().postDataJSON()?.text ?? '');
    const kind = c.expect.shift() ?? 'prompt';
    if (kind === 'fail') { c.failing.push(route); return; }
    if (kind === 'steer') { c.steers.add(text); openGate(c, gateKey(text)); } else c.gated.push(gateKey(text));
    await route.continue();
  });
  await page.route((u) => u.pathname === '/api/skills' || u.pathname === '/api/files', (route) => { c.lists.push(route); });
  await page.route((u) => u.pathname === '/api/attachments', (route) => {
    if (route.request().method() !== 'POST') return route.continue();
    c.upload.push(route);
  });
  await page.goto(`${serve.url}/#/s/${c.a}`);
  // The spec starts with A open and its composer taken: a click in it.
  await composer(c).click();
  await expect(page.locator('.composer-tools button[aria-label^="Next turn model"]')).toBeEnabled();

  const seen: string[] = [];
  const edges: string[] = [];
  let reached = 0;
  try {
    for (let n = 0; n < trace.length; n++) {
      const name = n === 0 ? 'Init' : bare(trace[n].action);
      const where = `step ${n} (${name})`;
      if (n > 0 && n <= reached) {
        // A Flush the page took on its own, already matched.
        seen.push(key(roleState(ROLE, trace[n].state)));
        edges.push(`${key(roleState(ROLE, trace[n - 1].state))} ${name} ${key(roleState(ROLE, trace[n].state))}`);
        continue;
      }
      if (n > 0) {
        const act = actions[name];
        if (!act) throw new Error(`${SPEC}: no action for ${trace[n].action}`);
        c.allow = null;
        await act(c, roleState(ROLE, trace[n - 1].state), roleState(ROLE, trace[n].state), trace, n);
      }
      const m = await settle(c, trace, n, where, errors);
      if (m < 0) {
        info.annotations.push({ type: 'not-taken', description: `${where}: the page flushed its queue first, which the spec allows` });
        break;
      }
      await invariants(c, errors, where);
      seen.push(key(roleState(ROLE, trace[n].state)));
      if (n > 0) edges.push(`${key(roleState(ROLE, trace[n - 1].state))} ${name} ${key(roleState(ROLE, trace[n].state))}`);
      reached = m;
    }
  } finally {
    recordCoverage(info, seen, edges);
    await page.goto('about:blank');
    for (const r of [...c.failing, ...c.lists, ...c.upload].splice(0)) await r.abort().catch(() => {});
    // Archiving kills the children, so nothing of this walk takes the
    // next walk's model turns; then every gate and turn it left goes.
    for (const id of [c.a, c.b]) await serve.api.post(`/api/sessions/${id}/archive`).catch(() => {});
    for (const k of c.gated) openGate(c, k);
    for (const f of fs.readdirSync(c.dir)) if (f.endsWith('.json')) fs.rmSync(path.join(c.dir, f), { force: true });
    if (c.held) releaseWith(c.dir, c.held, { mode: 'ok', text: 'cleanup' });
  }
}
