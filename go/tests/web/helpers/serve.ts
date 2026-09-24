// A real `bough serve` for a spec, on an isolated HOME. Two fixtures:
//
//   serve        one process per test. Seed files and a bough.yml with
//                test.use({ serveOpts }); nothing another test does can
//                reach it.
//   sharedServe  one process per worker (configured with
//                workerServeOpts). The boot is paid once; a test stays
//                isolated by working only in sessions it created
//                (newSession), never by asserting on the whole list.
//
// Either way the test's `page` is already signed in: the token cookie
// serve would set on a GET of "/" is added to the context up front, so a
// deep link (/#/projects/x) works as the first navigation. `api` is a
// request context with the bearer token and a same-origin Origin, which
// serve's CSRF guard wants on every POST. The server log is attached to
// the report when a test fails.
import { ChildProcess, spawn } from 'child_process';
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { APIRequestContext, BrowserContext, TestInfo, test as base } from '@playwright/test';
import { boughBin, freePort } from './bough';

export interface ServeOpts {
  /** Files under HOME before serve boots, relative path -> content. */
  home?: Record<string, string>;
  /** ~/.bough/bough.yml, which serve and every session it starts resolve.
   *  Defaults to an llm-echo row so no test reaches a provider. */
  config?: string;
  readyTimeoutMs?: number;
  /** Extra environment for serve and the sessions it starts. */
  env?: Record<string, string>;
}

export interface Serve {
  url: string;
  port: number;
  pid: number;
  home: string;
  /** An empty directory under HOME for local sessions to run in. */
  work: string;
  token: string;
  /** Bearer + Origin preset, baseURL = url. */
  api: APIRequestContext;
  /** Everything serve wrote to stdout+stderr so far. */
  output(): string;
  /** Create a local session in `work` and return its id. */
  newSession(prompt?: string): Promise<string>;
  /** SIGTERM, SIGKILL after 3s, remove HOME. Idempotent. */
  stop(): Promise<void>;
  /** The binary the current (or last) serve process runs. */
  bin(): string;
  /** SIGTERM and wait, as launchd or `bough update` stop it (SIGKILL
   *  after 5s); HOME, port and token stay for resume. */
  shutdown(): Promise<void>;
  /** Start serve again on the same HOME and port after shutdown, from
   *  bin (default: the one it last ran), and wait until it answers. */
  resume(bin?: string): Promise<void>;
}

const echoConfig = '- id: llm\n  plugin: llm-echo\n';

// Provider keys in the runner's environment would let a session that
// slipped past the echo row call out; serve never needs them here.
function hermeticEnv(home: string, extra: Record<string, string> = {}): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, HOME: home };
  for (const k of Object.keys(env)) if (k.endsWith('_API_KEY')) delete env[k];
  return { ...env, ...extra };
}

export async function startServe(
  newRequest: (o: { baseURL: string; extraHTTPHeaders: Record<string, string> }) => Promise<APIRequestContext>,
  opts: ServeOpts = {},
): Promise<Serve> {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-serve-'));
  const work = path.join(home, 'work');
  // Written before boot so there is no window where the file is
  // missing or half-written; serve adopts an existing token.
  const token = crypto.randomBytes(32).toString('hex');
  const files: Record<string, string> = {
    '.bough/serve.token': token + '\n',
    '.bough/bough.yml': opts.config ?? echoConfig,
    ...opts.home,
  };
  fs.mkdirSync(work, { recursive: true });
  for (const [rel, text] of Object.entries(files)) {
    const p = path.join(home, rel);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, text);
  }
  fs.chmodSync(path.join(home, '.bough', 'serve.token'), 0o600);

  const port = await freePort();
  const url = `http://127.0.0.1:${port}`;
  const chunks: string[] = [];
  const output = () => chunks.join('');
  // A restart spawns serve again on the same HOME and port: the child,
  // its exit and its binary are whichever process is current.
  let child!: ChildProcess;
  let exited!: Promise<void>;
  let bin = boughBin;
  const launch = (b: string) => {
    // cwd = HOME, which has no ./bough.yml, so ~/.bough/bough.yml is the one in force.
    const c = spawn(b, ['serve', '--run', `127.0.0.1:${port}`], { cwd: home, env: hermeticEnv(home, opts.env) });
    c.stdout?.on('data', (d) => chunks.push(String(d)));
    c.stderr?.on('data', (d) => chunks.push(String(d)));
    exited = new Promise<void>((resolve) => c.once('exit', () => resolve()));
    child = c;
    bin = b;
  };
  const alive = () => child.exitCode === null && child.signalCode === null;
  const terminate = async (graceMs: number) => {
    if (!alive()) return;
    const t = setTimeout(() => child.kill('SIGKILL'), graceMs);
    child.kill('SIGTERM');
    await exited;
    clearTimeout(t);
  };
  launch(boughBin);

  const api = await newRequest({ baseURL: url, extraHTTPHeaders: { Authorization: `Bearer ${token}`, Origin: url } });
  let stopped: Promise<void> | undefined;
  const stop = () => (stopped ??= (async () => {
    await api.dispose();
    await terminate(3000);
    fs.rmSync(home, { recursive: true, force: true });
  })());

  const readyMs = opts.readyTimeoutMs ?? 15_000;
  // undefined once serve answers /api/health, else why it did not.
  const waitReady = async (): Promise<string | undefined> => {
    const deadline = Date.now() + readyMs;
    let last = '';
    for (;;) {
      if (!alive()) return `bough serve exited (${child.exitCode ?? child.signalCode}) before it was ready:\n${output()}`;
      try {
        const res = await api.get('/api/health', { timeout: 1000 });
        if (res.status() === 200) return undefined;
        last = `health ${res.status()}`;
      } catch (e) {
        last = String(e);
      }
      if (Date.now() > deadline) return `bough serve not ready after ${readyMs}ms (${last}):\n${output()}`;
      await new Promise((r) => setTimeout(r, 50));
    }
  };

  const s: Serve = {
    url, port, get pid() { return child.pid ?? 0; }, home, work, token, api, output, stop,
    bin: () => bin,
    shutdown: () => terminate(5000),
    resume: async (b?: string) => {
      if (alive()) throw new Error('serve: resume while serve is still running');
      launch(b ?? bin);
      const err = await waitReady();
      if (err) throw new Error(err);
    },
    newSession: async (prompt = '') => {
      const res = await api.post('/api/sessions', { data: { cwd: work, prompt } });
      if (!res.ok()) throw new Error(`serve: create session: ${res.status()} ${await res.text()}`);
      return (await res.json()).session.id;
    },
  };

  const err = await waitReady();
  if (err) {
    await stop();
    throw new Error(err);
  }
  return s;
}

/** The cookie serve sets on a GET of "/", set before any navigation. */
export async function signIn(context: BrowserContext, s: Serve): Promise<void> {
  await context.addCookies([{ name: 'bough_serve_token', value: s.token, url: s.url, httpOnly: true, sameSite: 'Strict' }]);
}

async function attachLog(testInfo: TestInfo, log: string): Promise<void> {
  if (testInfo.status === testInfo.expectedStatus) return;
  await testInfo.attach('bough-serve-output', { body: log || '(no output)', contentType: 'text/plain' });
}

export const test = base.extend<
  { serveOpts: ServeOpts; serve: Serve; sharedServe: Serve },
  { workerServeOpts: ServeOpts; workerServe: Serve }
>({
  serveOpts: [{}, { option: true }],
  workerServeOpts: [{}, { option: true, scope: 'worker' }],

  serve: async ({ serveOpts, context, playwright }, use, testInfo) => {
    const s = await startServe((o) => playwright.request.newContext(o), serveOpts);
    try {
      await signIn(context, s);
      await use(s);
    } finally {
      await attachLog(testInfo, s.output());
      await s.stop();
    }
  },

  workerServe: [async ({ workerServeOpts, playwright }, use) => {
    const s = await startServe((o) => playwright.request.newContext(o), workerServeOpts);
    try {
      await use(s);
    } finally {
      await s.stop();
    }
  }, { scope: 'worker' }],

  // The per-test view of the worker's serve: signs this test's page in
  // and attaches only the log written while this test ran.
  sharedServe: async ({ workerServe, context }, use, testInfo) => {
    const from = workerServe.output().length;
    await signIn(context, workerServe);
    await use(workerServe);
    await attachLog(testInfo, workerServe.output().slice(from));
  },
});

export { expect } from '@playwright/test';
