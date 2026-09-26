// go/tests/model/specs/setup_wizard_concurrent_key_write.fizz in the
// browser: the welcome wizard's key save (POST /api/setup/key, provider
// anthropic, driven through the real welcome UI) racing a concurrent
// `bough setup`-style save (provider openai, no UI of its own — driven
// straight at /api/setup/key the way a second bough process reaches
// connect.WriteKey), both against the same ~/.bough/env.
//
// Acquire/Read carry no observable UI of their own (WriteKey's lock,
// read and write all happen inside one HTTP call), so they are tracked
// on the context exactly as go/tests/model/mbt/setup_wizard_concurrent_key_write_test.go's
// adapter tracks them; fileHasA/fileHasB are never taken from that
// bookkeeping, only read fresh off ~/.bough/env, so a dropped line would
// show up here the same way it would there.
import * as fs from 'fs';
import * as path from 'path';
import type { AddressInfo } from 'net';
import * as http from 'http';
import type { Page } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import { test, type Serve } from '../../helpers/serve';

// The key-check stub every SaveKey's automatic follow-up check hits,
// answered fast so it never leaves a slow real network call hanging off
// the walk (setup_welcome.spec.ts does the same).
const stub: Promise<string> = new Promise((resolve) => {
  const srv = http.createServer((_req, res) => { res.statusCode = 200; res.end(); });
  srv.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${(srv.address() as AddressInfo).port}`));
  srv.unref();
});

test.use({
  serveOpts: async ({}, use) => {
    await use({
      cwd: 'start',
      home: { 'start/.keep': '' },
      env: {
        BOUGH_SETUP_CHECK_URL: await stub,
        HTTPS_PROXY: 'http://127.0.0.1:9', HTTP_PROXY: 'http://127.0.0.1:9',
      },
    });
  },
});

interface Ctx {
  page: Page;
  serve: Serve;
  lock: string;
  welcomePhase: string;
  welcomeSeenB: boolean;
  cliPhase: string;
  cliSeenA: boolean;
  reloadedA: boolean;
  reloadedB: boolean;
}

// ~/.bough/env read fresh off disk, mirroring the Go adapter's
// fileState(): the one place a dropped line would show up.
function fileState(serve: Serve): { hasA: boolean; hasB: boolean } {
  let text = '';
  try {
    text = fs.readFileSync(path.join(serve.home, '.bough', 'env'), 'utf8');
  } catch {
    return { hasA: false, hasB: false };
  }
  let hasA = false, hasB = false;
  for (const raw of text.split('\n')) {
    const line = raw.trim().replace(/^export /, '');
    const eq = line.indexOf('=');
    if (eq < 0) continue;
    const k = line.slice(0, eq).trim();
    if (k === 'ANTHROPIC_API_KEY') hasA = true;
    if (k === 'OPENAI_API_KEY') hasB = true;
  }
  return { hasA, hasB };
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { hasA, hasB } = fileState(c.serve);
  return {
    lock: c.lock,
    fileHasA: hasA,
    fileHasB: hasB,
    welcomePhase: c.welcomePhase,
    welcomeSeenB: c.welcomeSeenB,
    cliPhase: c.cliPhase,
    cliSeenA: c.cliSeenA,
    reloadedA: c.reloadedA,
    reloadedB: c.reloadedB,
  };
}

modelTests<Ctx>({
  spec: 'setup_wizard_concurrent_key_write',
  role: 'Env#0',

  async init(page, serve) {
    const c: Ctx = {
      page, serve,
      lock: 'none',
      welcomePhase: 'idle', welcomeSeenB: false,
      cliPhase: 'idle', cliSeenA: false,
      reloadedA: false, reloadedB: false,
    };
    await page.goto(serve.url);
    return c;
  },

  actions: {
    // --- the welcome wizard's save (provider A) -------------------------
    async WelcomeAcquire(c) {
      c.lock = 'welcome';
      c.welcomePhase = 'reading';
    },
    async WelcomeRead(c) {
      c.welcomeSeenB = fileState(c.serve).hasB;
      c.welcomePhase = 'writing';
    },
    // The wizard's own UI: fill the key field and click Save, and wait
    // for the save to land before the step is called done.
    async WelcomeWrite(c) {
      const res = c.page.waitForResponse((r) => new URL(r.url()).pathname === '/api/setup/key');
      await c.page.getByLabel('Anthropic API key').fill('sk-ant-wizard');
      await c.page.getByRole('button', { name: 'Save key' }).click();
      if (!(await res).ok()) throw new Error('the wizard save failed');
      c.welcomePhase = 'done';
      c.lock = 'none';
    },

    // --- a concurrent `bough setup`-style save (provider B) -------------
    // No UI of its own (a second bough process in a terminal): reaches
    // the same /api/setup/key endpoint, and so the same connect.WriteKey,
    // as another client would.
    async CliAcquire(c) {
      c.lock = 'cli';
      c.cliPhase = 'reading';
    },
    async CliRead(c) {
      c.cliSeenA = fileState(c.serve).hasA;
      c.cliPhase = 'writing';
    },
    async CliWrite(c) {
      const res = await c.serve.api.post('/api/setup/key', { data: { provider: 'openai', key: 'sk-oa-cli' } });
      if (!res.ok()) throw new Error(`cli save: ${res.status()} ${await res.text()}`);
      c.cliPhase = 'done';
      c.lock = 'none';
    },

    // --- a hot env-file reload, unsynchronized with either writer -------
    async Reload(c) {
      const { hasA, hasB } = fileState(c.serve);
      c.reloadedA = hasA;
      c.reloadedB = hasB;
    },
  },

  read: readUiState,
  status: (c) => c.page.locator('.welcome'),
  sessions: () => [],
});
