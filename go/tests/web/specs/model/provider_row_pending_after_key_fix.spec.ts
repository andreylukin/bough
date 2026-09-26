// go/tests/model/specs/provider_row_pending_after_key_fix.fizz in the
// browser (recipe: go/tests/model/README.md): a provider row started
// with a bad key stays Failed until an explicit Remount, exercised
// through plugins/testgate exactly as the Go twin
// (go/tests/model/mbt/provider_row_pending_after_key_fix_test.go) drives
// it — "provider" reads a key file outside its config, "dependent"
// injects the service it provides, and "control" registers the
// `/gate-*` commands a session runs synchronously (never reaching the
// loop/LLM). None of this has a UI: a provider row and its dependent are
// not drawn anywhere a person looks, so every action here is the same
// POST /api/sessions/{id}/prompt the composer itself calls (bough's own
// dispatch treats a "/" line as a command whichever client sent it, per
// AGENTS.md's "behavior attaches as a row" — there is no separate web
// code path to test). The one thing this file adds over the Go run is
// that real network edge: CSRF-guarded POSTs through Playwright's own
// request context against a live serve, not an in-process client.
//
// What is read: Provider#0's fields exactly as the Go adapter's
// GetState reads them — status and dependentMounted off `/gate-rows`,
// keyValid and remounts tracked here the same way the Go adapter tracks
// them (neither is observable from outside: a key file's validity is
// only proven by a Remount succeeding, and remounts is a walk-local
// count). The session's sidebar row is the page's one visible carrier,
// standing in as the invariants' "status visible" element even though
// it never reflects Provider#0's own state (nothing on screen does).
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const providerConfig = '- id: llm\n  plugin: llm-echo\n' +
  '- id: provider\n  plugin: test-gate-provider\n' +
  '- id: dependent\n  plugin: test-gate-dependent\n' +
  '- id: control\n  plugin: test-gate-control\n';

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  keyfile: string;
  keyValid: boolean;
  remounts: number;
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
// serve's supervisor regardless of client, so this is what a person's
// composer does for a slash command, not a bypass of it.
async function command(c: Ctx, text: string): Promise<string> {
  const res = await c.serve.api.post(`/api/sessions/${c.id}/prompt`, { data: { text } });
  if (!res.ok()) throw new Error(`prompt: ${res.status()} ${await res.text()}`);
  return until(`reply to ${text}`, async () => {
    const r = await c.serve.api.get(`/api/sessions/${c.id}`);
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

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const m = await rows(c);
  return {
    status: m.provider === 'active' ? 'ready' : 'failed',
    keyValid: c.keyValid,
    dependentMounted: m.dependent === 'active',
    remounts: c.remounts,
  };
}

modelTests<Ctx>({
  spec: 'provider_row_pending_after_key_fix',
  role: 'Provider#0',
  config: providerConfig,

  async init(page, serve) {
    const keyfile = path.join(serve.home, '.bough', 'test-gate-key');
    fs.writeFileSync(keyfile, 'ok\n');
    const id = await serve.newSession();
    const c: Ctx = { page, serve, id, keyfile, keyValid: false, remounts: 0 };
    await page.goto(`${serve.url}/#/s/${id}`);
    await waitRows(c, (m) => m.provider === 'active' && m.dependent === 'active');
    fs.rmSync(keyfile);
    // A distinct command line, same as the Go adapter's Init, so this
    // bootstrap never reads as one of the walk's own Remount actions —
    // providerHistory (Go) and this file's own trace both key off the
    // exact text "/gate-remount provider".
    const out = await command(c, '/gate-remount provider setup');
    if (out !== 'remounted provider') throw new Error(`breaking the key: ${out}`);
    await waitRows(c, (m) => m.provider === 'failed' && m.dependent === 'pending');
    return c;
  },

  actions: {
    // No UI shows a Reconcile; /connect and /model use this same seam
    // (config-set with the row's own plugin) to fire it after a save.
    async ReconcileSameSpec(c) {
      await command(c, '/gate-reconcile');
    },
    // The /connect equivalent: fixes the credential outside the row's
    // config (a file, like a hand edit of ~/.bough/env), touching
    // nothing in the session itself.
    async FixKey(c) {
      fs.writeFileSync(c.keyfile, 'ok\n');
      await command(c, '/gate-fixkey');
      c.keyValid = true;
    },
    // kernel.Context.Remount: no UI offers it directly either (a real
    // session would learn this through /connect's own remount), so this
    // is the same call the Go twin makes.
    async Remount(c) {
      const out = await command(c, '/gate-remount provider');
      if (out !== 'remounted provider') throw new Error(`gate-remount provider: ${out}`);
      const m = await rows(c);
      if (m.provider === 'active') c.remounts++;
    },
  },

  read: readUiState,
  status: row,
  sessions: (c) => [c.id],
});
