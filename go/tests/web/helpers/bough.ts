// Launch helper: spawns one isolated bough --web process per call.
// Isolation: temp HOME, temp cwd, its own copy of bough.yml, its own
// port. llm-echo is forced via --set so no real API is ever hit.
import { ChildProcess, execFileSync, spawn } from 'child_process';
import * as fs from 'fs';
import * as net from 'net';
import * as os from 'os';
import * as path from 'path';

const repoRoot = path.resolve(__dirname, '..', '..', '..');

export const boughBin = process.env.BOUGH_BIN ?? path.join(repoRoot, 'bough');

export interface LaunchOpts {
  /** Extra --set overrides ("id.key=value"). llm.plugin=llm-echo is always applied first. */
  sets?: string[];
  /** Extra CLI args (e.g. ["-c"] or ["-r"]), appended before --web. */
  args?: string[];
  /** Files under the temp HOME, relative path -> content (e.g. ".bough/init.js"). */
  home?: Record<string, string>;
  /** Files under the temp cwd, relative path -> content (e.g. "AGENTS.md"). */
  cwd?: Record<string, string>;
  /** Replace the copied bough.yml with this content. */
  config?: string;
  /** How long to wait for /health (ms). */
  readyTimeoutMs?: number;
}

export interface Bough {
  url: string;
  port: number;
  home: string;
  cwd: string;
  configPath: string;
  proc: ChildProcess;
  /** Everything the process wrote to stdout+stderr so far. */
  output(): string;
  /** SIGTERM, wait for exit (SIGKILL after 3s). Idempotent. */
  kill(): Promise<void>;
  /** Run a bough subcommand (e.g. ["log"]) against this instance's HOME/cwd. */
  cli(args: string[]): string;
}

/** A free TCP port: listen on :0, close, reuse. Tiny race accepted. */
export function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      if (addr === null || typeof addr === 'string') {
        srv.close();
        reject(new Error('no address from listener'));
        return;
      }
      const port = addr.port;
      srv.close(() => resolve(port));
    });
    srv.on('error', reject);
  });
}

function writeTree(base: string, files: Record<string, string>): void {
  for (const [rel, content] of Object.entries(files)) {
    const p = path.join(base, rel);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, content);
  }
}

async function waitHealthy(b: Bough, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastErr = '';
  while (Date.now() < deadline) {
    if (b.proc.exitCode !== null) {
      throw new Error(`bough exited (code ${b.proc.exitCode}) before serving:\n${b.output()}`);
    }
    try {
      const res = await fetch(`${b.url}/health`);
      if (res.status === 200) return;
      lastErr = `health ${res.status}`;
    } catch (e) {
      lastErr = String(e);
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`bough not healthy after ${timeoutMs}ms (${lastErr}):\n${b.output()}`);
}

export async function launch(opts: LaunchOpts = {}): Promise<Bough> {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), 'bough-e2e-'));
  const home = path.join(base, 'home');
  const cwd = path.join(base, 'cwd');
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(cwd, { recursive: true });

  const configPath = path.join(cwd, 'bough.yml');
  if (opts.config !== undefined) {
    fs.writeFileSync(configPath, opts.config);
  } else {
    fs.copyFileSync(path.join(repoRoot, 'bough.yml'), configPath);
  }
  if (opts.home) writeTree(home, opts.home);
  if (opts.cwd) writeTree(cwd, opts.cwd);

  const port = await freePort();
  const sets = ['llm.plugin=llm-echo', ...(opts.sets ?? [])];
  const args = ['--config', 'bough.yml'];
  for (const s of sets) args.push('--set', s);
  args.push(...(opts.args ?? []));
  args.push('--web', `127.0.0.1:${port}`);

  const env = { ...process.env, HOME: home };
  const proc = spawn(boughBin, args, { cwd, env });
  const chunks: string[] = [];
  proc.stdout?.on('data', (d) => chunks.push(String(d)));
  proc.stderr?.on('data', (d) => chunks.push(String(d)));

  let killed = false;
  const exited = new Promise<void>((resolve) => proc.on('exit', () => resolve()));

  const b: Bough = {
    url: `http://127.0.0.1:${port}`,
    port,
    home,
    cwd,
    configPath,
    proc,
    output: () => chunks.join(''),
    kill: async () => {
      if (killed) {
        await exited;
        return;
      }
      killed = true;
      if (proc.exitCode === null) {
        proc.kill('SIGTERM');
        const t = setTimeout(() => proc.kill('SIGKILL'), 3000);
        await exited;
        clearTimeout(t);
      }
    },
    cli: (cliArgs: string[]) =>
      execFileSync(boughBin, cliArgs, { cwd, env, encoding: 'utf8' }),
  };

  try {
    await waitHealthy(b, opts.readyTimeoutMs ?? 15_000);
  } catch (e) {
    await b.kill();
    throw e;
  }
  return b;
}
