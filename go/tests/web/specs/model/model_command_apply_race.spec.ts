// go/tests/model/specs/model_command_apply_race.fizz in the browser: two
// callers racing the model picker's POST /api/sessions/{id}/model on one
// session, where the save (meta.Model, what the picker shows at once) and
// the deliver (the "/model" line reaching the child's stdin, what the row
// actually runs on) are two separate steps a second caller's own pick can
// land between (internal/serve/supervisor.go's pick).
//
// Neither caller's target ("m1"/"m2") is a model the catalogue lists, so
// no UI names it (as model-effort-controls.spec.ts's SetUnlistedModel):
// both Pick1 and Pick2 are another client's POST. What the race needs —
// the two callers' sends landing in a chosen order, not whatever a real
// race gives on this machine — is the same BOUGH_TEST_STEP_GATE hook the
// Go adapter drives (mbt/model_command_apply_race_test.go): each POST
// carries a `race=pick-<tag>` tag, Supervisor.pick holds on it between its
// save and its Send, and this spec's Deliver1/Deliver2 release one at a
// time, in whatever order a walk picks.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Locator, Page } from '@playwright/test';
import { controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const RACE_CONFIG = '- id: llm\n  plugin: llm-control\n  config:\n    model: m1\n';

// One gate directory for the whole worker: BOUGH_TEST_STEP_GATE also
// holds every stdin line a child reads (plugins/ui/headless.go's hlGate,
// "in.<n>"), which this spec must let through itself (autoReleaseStdin)
// since the same process-wide dir carries the "pick-*" holds the walks
// control by hand.
const GATE_DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-model-race-gate-'));

let stdinReleaseStarted = false;
function autoReleaseStdin(): void {
  if (stdinReleaseStarted) return;
  stdinReleaseStarted = true;
  setInterval(() => {
    let entries: string[] = [];
    try {
      entries = fs.readdirSync(GATE_DIR);
    } catch {
      return;
    }
    for (const e of entries) {
      if (!e.startsWith('in.') || !e.endsWith('.held')) continue;
      const go = path.join(GATE_DIR, e.slice(0, -'.held'.length) + '.go');
      if (!fs.existsSync(go)) fs.writeFileSync(go, '');
    }
  }, 20);
}

function waitFile(p: string, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const poll = () => {
      if (fs.existsSync(p)) return resolve();
      if (Date.now() > deadline) return reject(new Error(`timed out waiting for ${p}`));
      setTimeout(poll, 5);
    };
    poll();
  });
}

// A SetModel call parked on the step gate between its save and its
// deliver: startPick waits for that hold (the save has landed) before
// returning, and releasePick lets the deliver through.
interface Pending { tag: string; target: string; done: Promise<void> }

async function startPick(c: Ctx, target: string): Promise<Pending> {
  c.seq += 1;
  const tag = `c${c.seq}`;
  const done = c.serve.api.post(`/api/sessions/${c.id}/model?race=pick-${tag}`, { data: { model: target } }).then((res) => {
    if (!res.ok()) throw new Error(`model race ${tag}: ${res.status()}`);
  });
  await waitFile(path.join(GATE_DIR, `pick-${tag}.held`));
  return { tag, target, done };
}

function releasePick(p: Pending): void {
  fs.writeFileSync(path.join(GATE_DIR, `pick-${p.tag}.go`), '');
}

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  held: string; // the llm-control turn in flight, '' when none
  turn: number;
  seq: number; // race tags are unique across a walk
  req1: string; // caller 1's saved-not-yet-delivered choice, '' when none
  req2: string;
  pend1?: Pending;
  pend2?: Pending;
  turnModel: string; // applied, snapshotted when a turn ends (see Finish)
}

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();
const modelBtn = (c: Ctx) =>
  c.page.locator('.composer-tools .sel').filter({ has: c.page.locator('button[aria-label^="Next turn model"]') }).locator('button.sel-btn');

// The Select button falls back to the raw value when it names no option
// the catalogue lists — exactly the case here, since "m1"/"m2" are never
// in it: app.tsx's Controls still sets `value = row.model` and the
// button's accessible name is "<label>: <value>".
async function buttonValue(b: Locator): Promise<string> {
  const name = (await b.getAttribute('aria-label')) ?? '';
  return name.slice(name.indexOf(': ') + 2);
}

// The row's status word (example.spec.ts's WORDS): this flow's turns
// always finish clean, so only Idle/Running appear.
const WORDS: Record<string, string> = { Idle: 'idle', Running: 'running' };

// The most recent "Model changed" block's detail text is render.tsx's
// foldModelSwitch output, "<plugin> · <model>" (app.tsx's model-switch
// block) — the same "model: …" system line internal/serve/api.go's
// lastModel and this flow's Go adapter (appliedModel) read, one DOM hop
// removed. "" before any switch has landed.
async function appliedFromDom(c: Ctx): Promise<string> {
  const labels = c.page.locator('.block-label', { hasText: 'Model changed' });
  const n = await labels.count();
  if (n === 0) return '';
  const text = (await labels.nth(n - 1).locator('xpath=following-sibling::span[1]').textContent()) ?? '';
  const at = text.indexOf(' · ');
  return at === -1 ? '' : text.slice(at + 3);
}

async function waitApplied(c: Ctx, want: string): Promise<void> {
  const deadline = Date.now() + 10_000;
  for (;;) {
    if ((await appliedFromDom(c)) === want) return;
    if (Date.now() > deadline) throw new Error(`model_command_apply_race: applied never reached ${want}`);
    await new Promise((r) => setTimeout(r, 20));
  }
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const r = row(c);
  const label = (await r.getAttribute('aria-label')) ?? '';
  const parts = label.split(', ');
  return {
    model: await buttonValue(modelBtn(c)),
    applied: await appliedFromDom(c),
    turnModel: c.turnModel,
    status: WORDS[parts[1]] ?? `unknown: ${label}`,
    req1: c.req1,
    req2: c.req2,
  };
}

modelTests<Ctx>({
  spec: 'model_command_apply_race',
  role: 'Session#0',
  config: RACE_CONFIG,
  env: { BOUGH_TEST_STEP_GATE: GATE_DIR },
  shared: true,
  reset: true,

  async init(page, serve) {
    autoReleaseStdin();
    const c: Ctx = { page, serve, id: await serve.newSession(), held: '', turn: 0, seq: 0, req1: '', req2: '', turnModel: 'm1' };
    // An ordinary (untagged) /model m1, same as the Go adapter's Init:
    // the row's own config already answers as "m1", but nothing has told
    // the picker that yet.
    const res = await serve.api.post(`/api/sessions/${c.id}/model`, { data: { model: 'm1' } });
    if (!res.ok()) throw new Error(`model_command_apply_race: init /model: ${res.status()} ${await res.text()}`);
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await modelBtn(c).waitFor();
    await waitApplied(c, 'm1');
    return c;
  },

  actions: {
    // save: meta.Model updates at once, for anyone polling the row; no UI
    // names a model the catalogue does not list, so this is another
    // client's POST, tagged so it parks on the step gate right after its
    // save, before its deliver.
    async Pick1(c) {
      const cur = await buttonValue(modelBtn(c));
      const target = cur === 'm1' ? 'm2' : 'm1';
      c.pend1 = await startPick(c, target);
      c.req1 = target;
    },
    async Pick2(c) {
      const cur = await buttonValue(modelBtn(c));
      const target = cur === 'm1' ? 'm2' : 'm1';
      c.pend2 = await startPick(c, target);
      c.req2 = target;
    },
    // deliver: the stdin write lands; the row reconfigures live, whether
    // or not a turn is running.
    async Deliver1(c) {
      const p = c.pend1!;
      releasePick(p);
      await p.done;
      c.pend1 = undefined;
      c.req1 = '';
      await waitApplied(c, p.target);
    },
    async Deliver2(c) {
      const p = c.pend2!;
      releasePick(p);
      await p.done;
      c.pend2 = undefined;
      c.req2 = '';
      await waitApplied(c, p.target);
    },
    async Prompt(c) {
      const name = `t${String(++c.turn).padStart(4, '0')}`;
      const dir = controlDir(c.serve.home);
      queue(dir, name, { mode: 'block', text: `finished ${name}` });
      await c.page.locator('#composer').fill(`turn ${name}`);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      c.held = name;
      await waitTaken(dir, name);
    },
    async Finish(c) {
      release(controlDir(c.serve.home), c.held);
      c.held = '';
      await expectIdle(c);
      // The one field with no API of its own: the spec defines it as
      // applied snapshotted when a turn ends, so it is read off the DOM
      // exactly then, the same way example.spec.ts tracks `viewing`.
      c.turnModel = await appliedFromDom(c);
    },
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.held) release(controlDir(c.serve.home), c.held);
    if (c.pend1) {
      releasePick(c.pend1);
      await c.pend1.done.catch(() => {});
    }
    if (c.pend2) {
      releasePick(c.pend2);
      await c.pend2.done.catch(() => {});
    }
  },
});

async function expectIdle(c: Ctx): Promise<void> {
  const deadline = Date.now() + 10_000;
  for (;;) {
    const label = (await row(c).getAttribute('aria-label')) ?? '';
    if (label.split(', ')[1] === 'Idle') return;
    if (Date.now() > deadline) throw new Error(`model_command_apply_race: row never went idle (${label})`);
    await new Promise((r) => setTimeout(r, 20));
  }
}
