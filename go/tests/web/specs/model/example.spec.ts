// The worked example of a model-based browser spec (recipe:
// go/tests/model/README.md): go/tests/model/specs/example.fizz, one
// session's lifecycle as the control room shows it. Every generated path
// is walked through the page against a real serve whose model is
// llm-control, and at each node readUiState must equal the spec's state.
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;     // the session the spec is about
  decoy: string;  // somewhere else to look: Leave opens it
  turn: number;
  held: string;   // the llm-control turn in flight, '' when none
}

// The sidebar row: the one surface that shows the session whether or not
// it is open. A failure also pins a copy under "Needs you"; both read the
// same, so the first will do.
const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();

// The accessible label is "<title>, <state>[, not seen yet], <age> ago…";
// a failure carries its reason instead of a state word and is drawn with
// the red second line (row-meta-bad). Titles here are "turn tNNNN", so
// the commas are the label's own.
const WORDS: Record<string, string> = { Idle: 'idle', Running: 'running', Done: 'done' };

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const r = row(c);
  const label = (await r.getAttribute('aria-label')) ?? '';
  const parts = label.split(', ');
  const failed = (await r.locator('.row-meta-bad').count()) > 0;
  const hash = await c.page.evaluate(() => window.location.hash);
  return {
    status: failed ? 'error' : WORDS[parts[1]] ?? `unknown: ${label}`,
    unseen: parts.includes('not seen yet'),
    viewing: hash === `#/s/${c.id}` && (await r.getAttribute('aria-current')) === 'true',
  };
}

// Ends the held turn; the page, not the test, acks a finish it is showing.
function end(c: Ctx, fail: boolean): Promise<void> {
  const dir = controlDir(c.serve.home);
  if (fail) releaseWith(dir, c.held, { mode: 'error', error: 'model says no' });
  else release(dir, c.held);
  c.held = '';
  return Promise.resolve();
}

modelTests<Ctx>({
  spec: 'example',
  role: 'Session#0',
  config: CONTROL_CONFIG,
  shared: true,
  reset: true,

  async init(page, serve) {
    const c: Ctx = { page, serve, id: await serve.newSession(), decoy: await serve.newSession(), turn: 0, held: '' };
    // Start on the decoy: "/" alone opens the most recent session itself.
    await page.goto(`${serve.url}/#/s/${c.decoy}`);
    return c;
  },

  actions: {
    // The composer when the session is open; otherwise the same POST
    // from another client, since a closed session has no composer.
    async Prompt(c) {
      const name = `t${String(++c.turn).padStart(4, '0')}`;
      const dir = controlDir(c.serve.home);
      queue(dir, name, { mode: 'block', text: `finished ${name}` });
      if ((await readUiState(c)).viewing) {
        await c.page.locator('#composer').fill(`turn ${name}`);
        await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      } else {
        const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text: `turn ${name}` } });
        if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
      }
      c.held = name;
      await waitTaken(dir, name);
    },
    Finish: (c) => end(c, false),
    Fail: (c) => end(c, true),
    View: (c) => row(c).click(),
    Leave: (c) => c.page.locator(`button.row[data-id="${c.decoy}"]`).first().click(),
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.held) release(controlDir(c.serve.home), c.held);
  },
});
