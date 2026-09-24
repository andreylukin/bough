// The test side of llm-control (go/plugins/llm/control.go), the model a
// spec steers turn by turn; the Go twin is go/tests/model/llm. Select it
// with CONTROL_CONFIG as the serve's bough.yml. Each queued file is the
// model's next response; a "block" turn holds the session in "running"
// until the spec releases it, as a success or as an error.
import * as fs from 'fs';
import * as path from 'path';

export const CONTROL_CONFIG = '- id: llm\n  plugin: llm-control\n';

export interface Turn {
  mode: 'ok' | 'error' | 'slow' | 'block';
  text?: string;
  error?: string;
  delay_ms?: number;
}

/** The control dir the row reads when its config names none. */
export const controlDir = (home: string) => path.join(home, '.bough', 'llm-control');

// Writes go through a temp name the row ignores, so it never reads half
// a file (and never reads a half-written release as a plain one).
function put(file: string, body: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file + '-tmp', body);
  fs.renameSync(file + '-tmp', file);
}

/** Queue turn as <name>.json; names are taken in lexical order. */
export function queue(dir: string, name: string, turn: Turn): void {
  put(path.join(dir, name + '.json'), JSON.stringify(turn));
}

/** Let a "block" turn reply with its own text. */
export function release(dir: string, name: string): void {
  put(path.join(dir, name + '.release'), '');
}

/** Let a "block" turn reply as turn says instead ({mode: 'error'} fails it). */
export function releaseWith(dir: string, name: string, turn: Turn): void {
  put(path.join(dir, name + '.release'), JSON.stringify(turn));
}

/** Wait until the row has picked up turn name: the model call is in flight. */
export async function waitTaken(dir: string, name: string, timeoutMs = 20_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!fs.existsSync(path.join(dir, name + '.taken'))) {
    if (Date.now() > deadline) throw new Error(`llm-control: turn ${name} not taken after ${timeoutMs}ms`);
    await new Promise((r) => setTimeout(r, 20));
  }
}
