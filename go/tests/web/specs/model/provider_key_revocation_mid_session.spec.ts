// go/tests/model/specs/provider_key_revocation_mid_session.fizz in the
// browser (recipe: go/tests/model/README.md): a provider key that is
// valid when a turn starts is revoked externally while that turn is
// still open. This combines example.spec.ts's real model turn (llm row
// on llm-control, driven through the composer exactly as a person would)
// with provider_row_pending_after_key_fix.spec.ts's plugins/testgate
// seam for the provider row (a real credential-outside-config file, the
// `/gate-*` commands a session runs synchronously), the same two halves
// the Go twin (go/tests/model/mbt/provider_key_revocation_mid_session_test.go)
// wires in one serve. Neither the provider row nor its dependent has a
// UI, so RevokeKey/HealthCheckFails/SettleFailed/ReconcileSameSpec go
// through the same POST /api/sessions/{id}/prompt the composer itself
// calls for a "/" line — there is no separate web code path to test, per
// AGENTS.md's "behavior attaches as a row".
//
// What is read: Provider#0's fields exactly as the Go adapter's GetState
// reads them. rowStatus is proven real only at the one place a real
// answer exists (SettleFailed's `/gate-remount provider` against
// `/gate-rows`) and tracked here the rest of the time, the same as the
// Go adapter's own field; keyRevoked, open, req, closed and dones have
// no observable DOM signal distinct from the turn's own running/done/
// error word (streaming vs. merely sent, in particular, draws no
// differently), so they are tracked here exactly as the Go adapter
// tracks them, updated only where the real turn (composer, Send,
// llm-control's release) or the real remount actually happened.
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, releaseWith, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const pkrConfig = CONTROL_CONFIG +
  '- id: provider\n  plugin: test-gate-provider\n' +
  '- id: dependent\n  plugin: test-gate-dependent\n' +
  '- id: control\n  plugin: test-gate-control\n';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  ctrlId: string; // a second session, used only for /gate-* commands: the
                   // turn session can be busy (open, streaming) when
                   // SettleFailed fires (the row settles independently
                   // of any turn per the spec), and a session serializes
                   // its own prompts, so the control traffic needs a
                   // session of its own, never the one under test.
  keyfile: string;
  turn: number;
  held: string; // the llm-control turn in flight, '' when none

  rowStatus: 'ready' | 'pending' | 'failed';
  keyRevoked: boolean;
  open: boolean;
  req: '' | 'out' | 'streaming';
  closed: '' | 'done' | 'error';
  dones: number;
}

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();

async function until<T>(what: string, fn: () => Promise<T | undefined>, ms = 15_000): Promise<T> {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v !== undefined) return v;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 20));
  }
}

// The composer's own POST: a "/" line is dispatched as a command by
// serve's supervisor regardless of client (provider_row_pending_after_key_fix.spec.ts).
async function command(c: Ctx, text: string): Promise<string> {
  const res = await c.serve.api.post(`/api/sessions/${c.ctrlId}/prompt`, { data: { text } });
  if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
  return until(`reply to ${text}`, async () => {
    const r = await c.serve.api.get(`/api/sessions/${c.ctrlId}`);
    if (!r.ok()) throw new Error(`session: ${r.status()}`);
    const { entries } = (await r.json()) as { entries: { kind: string; text: string }[] };
    const n = entries.length;
    if (n >= 2 && entries[n - 1].kind === 'system' && entries[n - 2].kind === 'command' && entries[n - 2].text === text) {
      return entries[n - 1].text;
    }
    return undefined;
  });
}

async function rows(c: Ctx): Promise<Record<string, string>> {
  return JSON.parse(await command(c, '/gate-rows'));
}

async function waitRows(c: Ctx, ok: (m: Record<string, string>) => boolean): Promise<Record<string, string>> {
  return until('row states to settle', async () => {
    const m = await rows(c);
    return ok(m) ? m : undefined;
  });
}

// llm-control's own chunk-file protocol (go/tests/model/llm/control.go's
// Stream), not exposed by helpers/control.ts: writes <name>.stream via a
// temp-then-rename (the row never reads half a file) and waits for the
// row to consume it.
async function streamChunk(dir: string, name: string, text: string): Promise<void> {
  const dst = path.join(dir, `${name}.stream`);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(`${dst}-tmp`, text);
  fs.renameSync(`${dst}-tmp`, dst);
  await until(`${name}.stream consumed`, async () => (fs.existsSync(dst) ? undefined : true));
}

function readUiState(c: Ctx): Record<string, unknown> {
  return {
    rowStatus: c.rowStatus,
    keyRevoked: c.keyRevoked,
    open: c.open,
    req: c.req,
    closed: c.closed,
    dones: c.dones,
  };
}

modelTests<Ctx>({
  spec: 'provider_key_revocation_mid_session',
  role: 'Provider#0',
  config: pkrConfig,

  async init(page, serve) {
    const keyfile = path.join(serve.home, '.bough', 'test-gate-key');
    fs.writeFileSync(keyfile, 'ok\n');
    const id = await serve.newSession();
    const ctrlId = await serve.newSession();
    const c: Ctx = {
      page, serve, id, ctrlId, keyfile, turn: 0, held: '',
      rowStatus: 'ready', keyRevoked: false, open: false, req: '', closed: '', dones: 0,
    };
    await page.goto(`${serve.url}/#/s/${id}`);
    await waitRows(c, (m) => m.provider === 'active' && m.dependent === 'active');
    return c;
  },

  actions: {
    // The composer, exactly as example.spec.ts's Prompt: the session is
    // the one navigated to, so it is always the one being viewed.
    async StartTurn(c) {
      const name = `t${String(++c.turn).padStart(4, '0')}`;
      const dir = controlDir(c.serve.home);
      queue(dir, name, { mode: 'block', text: `finished ${name}` });
      await c.page.locator('#composer').fill(`turn ${name}`);
      await c.page.getByRole('button', { name: 'Send', exact: true }).click();
      c.held = name;
      await waitTaken(dir, name);
      c.open = true;
      c.req = 'out';
      c.closed = '';
      c.dones = 0;
    },

    async Stream(c) {
      await streamChunk(controlDir(c.serve.home), c.held, `chunk ${c.held}`);
      c.req = 'streaming';
    },

    // The external fact: a hand edit of ~/.bough/env, an org rotation, a
    // suspension. It touches only the key file, never the session or the
    // row directly.
    async RevokeKey(c) {
      fs.rmSync(c.keyfile, { force: true });
      c.keyRevoked = true;
    },

    // The open turn's own request discovers the revoked key: a 401 mid-
    // turn, closed as its own error in the same step, no different from
    // any other model failure a person would see.
    async RequestFailsRevoked(c) {
      releaseWith(controlDir(c.serve.home), c.held, { mode: 'error', error: 'ANTHROPIC_API_KEY was rejected (HTTP 401)' });
      c.held = '';
      c.open = false;
      c.req = '';
      c.closed = 'error';
      c.dones++;
      c.rowStatus = 'pending';
    },

    // The turn's own request went out and came back before the
    // revocation took effect on the provider side.
    async FinishTurnCleanly(c) {
      release(controlDir(c.serve.home), c.held);
      c.held = '';
      c.open = false;
      c.req = '';
      c.closed = 'done';
      c.dones++;
    },

    // No turn is open: nothing discovers the revocation until something
    // else tries to use the row (the next Remount). This never touches
    // turn state, because there is none to touch.
    async HealthCheckFails(c) {
      c.rowStatus = 'pending';
    },

    // The real kernel.Context.Remount that discovers the broken key: the
    // row's own Apply reads the (now missing) key file and fails.
    async SettleFailed(c) {
      const out = await command(c, '/gate-remount provider');
      if (out !== 'remounted provider') throw new Error(`gate-remount provider: ${out}`);
      const after = await rows(c);
      if (after.provider !== 'failed') throw new Error(`provider row did not fail on a revoked key: ${JSON.stringify(after)}`);
      c.rowStatus = 'failed';
    },

    // A Failed row stays Failed through a same-spec Reconcile.
    async ReconcileSameSpec(c) {
      await command(c, '/gate-reconcile');
    },
  },

  read: async (c) => readUiState(c),
  status: row,
  sessions: (c) => [c.id, c.ctrlId],
  async cleanup(c) {
    if (c.held) release(controlDir(c.serve.home), c.held);
  },
});
