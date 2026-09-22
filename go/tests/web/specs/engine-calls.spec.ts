// An engine session's native calls in the control room, against a real
// `bough serve` on an isolated HOME (go/docs/unreal-engine.md §10.4).
//
// The first test reads a recorded engine transcript, so it needs nothing
// but the binary: each call is a row that opens onto its output, a
// failure is marked on its own row, a call that outlived its turn says
// so on that turn's footer, and the turn it woke opens on a quiet line
// instead of a user bubble.
//
// The second drives a live session on engine-unreal + llm-script and
// watches one call go from its running row (spinner, live tail) to its
// final row. It skips itself when this binary has no engine-unreal row
// that mounts, so it runs once the engine lands and is inert before.
import { execFileSync, spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { test as base, expect } from '../helpers/fixtures';
import { boughBin, freePort } from '../helpers/bough';

const SID = '2026-09-22T10-00-00-00001';
const jsonl = (...entries: unknown[]) => entries.map((e) => JSON.stringify(e)).join('\n') + '\n';
const t = (s: number) => new Date(Date.UTC(2026, 8, 22, 10, 0, s)).toISOString();

// What the projector records for an engine turn: string call ids, no code
// blocks, a done that left one call running, and the wake turn it started.
const transcript = jsonl(
  { seq: 1, at: t(0), kind: 'meta', data: { cwd: '/w' } },
  { seq: 2, at: t(0), kind: 'input', data: { text: 'Run the tests and build in the background' } },
  { seq: 3, at: t(1), kind: 'engine', data: { engine: 'unreal', model: 'claude-opus-5-5', provider: 'anthropic' } },
  { seq: 4, at: t(4), kind: 'call', data: { text: 'go test ./...', id: 'toolu_a', tool: 'bash', ms: 3000, exit: 1, cmd: 'go test ./...', error: 'exit status 1', output: '--- FAIL: TestParse\nFAIL\tpkg/parse\nSENTINEL_TEST_OUTPUT', hseq: 5 } },
  { seq: 5, at: t(5), kind: 'call', data: { text: 'go/parse.go', id: 'toolu_b', tool: 'patch', ms: 20, add: 2, del: 1, output: 'patched go/parse.go', hseq: 7 } },
  { seq: 6, at: t(6), kind: 'assistant', data: { text: 'Fixed the parser; the build runs on in the background.', model: 'claude-opus-5-5', provider: 'anthropic', hseq: 8 } },
  { seq: 7, at: t(66), kind: 'job', data: { id: 1, event: 'started', cmd: 'make build', call: 'toolu_c' } },
  { seq: 8, at: t(66), kind: 'done', data: { files: ['go/parse.go'], running: 1, engine_turn: 3 } },
  { seq: 9, at: t(90), kind: 'call', data: { text: 'make build', id: 'toolu_c', tool: 'bash', ms: 85000, exit: 0, cmd: 'make build', job: 1, adopted: true, output: 'SENTINEL_BUILD_OUTPUT', hseq: 11 } },
  { seq: 10, at: t(90), kind: 'job', data: { id: 1, event: 'finished', exit: 0, call: 'toolu_c' } },
  { seq: 11, at: t(91), kind: 'input', data: { text: '[background call toolu_c finished]', wake: true, reason: 'call', calls: ['toolu_c'] } },
  { seq: 12, at: t(92), kind: 'assistant', data: { text: 'The build passed.', hseq: 13 } },
  { seq: 13, at: t(93), kind: 'done', data: { wake: true } },
);

interface Serve { url: string; home: string; cwd: string }

const test = base.extend<{ serve: Serve; overlay: string }>({
  overlay: ['- id: llm\n  plugin: llm-echo\n', { option: true }],
  serve: async ({ request, overlay }, use) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-engine-'));
    const home = path.join(root, 'home');
    const cwd = path.join(root, 'repo');
    fs.mkdirSync(path.join(cwd, '.git'), { recursive: true });
    fs.mkdirSync(path.join(home, '.bough', 'history'), { recursive: true });
    fs.writeFileSync(path.join(home, '.bough', 'history', `${SID}.jsonl`), transcript);
    fs.writeFileSync(path.join(home, '.bough', 'bough.yml'), overlay.replaceAll('$ROOT', root));
    const addr = `127.0.0.1:${await freePort()}`;
    const env: NodeJS.ProcessEnv = { ...process.env, HOME: home };
    for (const k of ['ANTHROPIC_API_KEY', 'OPENAI_API_KEY', 'OPENROUTER_API_KEY', 'CEREBRAS_API_KEY']) delete env[k];
    const child = spawn(boughBin, ['serve', '--run', addr], { cwd, env, stdio: 'ignore' });
    try {
      await expect.poll(async () => {
        try {
          await request.get(`http://${addr}/`);
          return (await request.get(`http://${addr}/api/health`)).status();
        } catch { return 0; }
      }).toBe(200);
      await use({ url: `http://${addr}`, home, cwd });
    } finally {
      if (child.exitCode === null && child.signalCode === null) {
        await new Promise<void>((resolve) => {
          const timer = setTimeout(() => child.kill('SIGKILL'), 3000);
          child.once('exit', () => { clearTimeout(timer); resolve(); });
          child.kill('SIGTERM');
        });
      }
      fs.rmSync(root, { recursive: true, force: true });
    }
  },
});

test('a recorded engine turn: call rows open onto output, a wake turn is a quiet line', async ({ page, serve }) => {
  await page.goto(`${serve.url}/#/s/${SID}`);
  await expect(page.getByText('Fixed the parser; the build runs on in the background.')).toBeVisible();

  // The engine's build record is provenance, not conversation.
  await expect(page.locator('.turn').getByText('unreal', { exact: true })).toHaveCount(0);

  // The turn that left a call running says so, and leads to Work.
  await expect(page.getByRole('button', { name: '1 call still running' })).toBeVisible();

  // The wake turn opens on a quiet line, holding the call that finished.
  const wake = page.locator('.call-wake');
  await expect(wake).toContainText('A background call finished');
  await expect(page.locator('.prompt-bubble')).toHaveCount(1);

  // The failed call is its own row: the command, marked, with its exit.
  const failed = page.locator('details.call-native.block-failed');
  await expect(failed).toHaveCount(1);
  await expect(failed.locator('summary')).toContainText('go test ./...');
  await expect(failed.locator('summary')).toContainText('exit 1');

  // The background build's row is marked as the job it became, and its output is one click in.
  const build = page.locator('details.call-native').filter({ hasText: 'make build' });
  await expect(build.locator('summary')).toContainText('Job 1');
  await expect(page.getByText('SENTINEL_BUILD_OUTPUT')).toBeHidden();
  // The row may sit inside a folded stretch of work: open whatever holds it, then the row itself.
  await build.evaluate((el) => { for (let d = el.parentElement?.closest('details'); d; d = d.parentElement?.closest('details')) (d as HTMLDetailsElement).open = true; });
  await build.locator('summary').click();
  await expect(page.getByText('SENTINEL_BUILD_OUTPUT')).toBeVisible();
});

// The live half needs an engine-unreal row that mounts in this binary.
function engineMounts(): boolean {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-engine-probe-'));
  try {
    execFileSync(boughBin, ['--headless', '--set', 'loop.plugin=engine-unreal', '--set', 'llm.plugin=llm-echo'], {
      cwd: home, env: { ...process.env, HOME: home }, input: '', timeout: 20_000, stdio: ['pipe', 'pipe', 'pipe'],
    });
    return true;
  } catch {
    return false;
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
}

const tape = JSON.stringify({
  steps: [
    { calls: [{ id: 'toolu_live', name: 'bash', args: { command: 'for i in 1 2 3 4; do echo LIVE_TICK_$i; sleep 1; done' } }] },
    { text: 'All four ticks printed.' },
  ],
});

test.describe('live engine session', () => {
  test.use({ overlay: '- id: loop\n  plugin: engine-unreal\n- id: llm\n  plugin: llm-script\n  script: $ROOT/tape.json\n' });

  test('a native call runs as a spinning row with its live tail, then settles into its final row', async ({ page, serve }) => {
    test.skip(!engineMounts(), 'this binary has no engine-unreal row that mounts');
    fs.writeFileSync(path.join(path.dirname(serve.home), 'tape.json'), tape);
    await page.goto(serve.url);
    const id: string = await page.evaluate(async (cwd) => {
      const r = await fetch('/api/sessions', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ cwd, prompt: 'tick four times' }) });
      return (await r.json()).session.id;
    }, serve.cwd);
    await page.goto(`${serve.url}/#/s/${id}`);

    const row = page.locator('details.call-native').filter({ hasText: 'LIVE_TICK' }).first();
    // Running: the present tense, a spinner, and the output so far.
    await expect(row.locator('summary .block-label')).toHaveText('Running', { timeout: 20_000 });
    await expect(row.locator('.spin-mark')).toBeVisible();
    await expect(row.locator('.call-tail')).toContainText('LIVE_TICK_', { timeout: 10_000 });
    // Finished: past tense, no spinner, and the whole output one click in.
    await expect(page.getByText('All four ticks printed.')).toBeVisible({ timeout: 20_000 });
    const done = page.locator('details.call-native').filter({ hasText: 'LIVE_TICK' }).first();
    await expect(done.locator('summary .block-label')).toHaveText('Ran');
    await expect(done.locator('.spin-mark')).toHaveCount(0);
    await done.evaluate((el) => { for (let d: HTMLElement | null = el as HTMLElement; d; d = d.parentElement?.closest('details') ?? null) (d as HTMLDetailsElement).open = true; });
    await expect(done.locator('.tool-output')).toContainText('LIVE_TICK_4');
  });
});
