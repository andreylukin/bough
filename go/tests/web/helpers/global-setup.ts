// Resolve the bough binary ONCE per suite run: the Go suites' cached
// build (go/internal/testbin), so a web run after `go test` — or a
// rerun with unchanged sources — links nothing. BOUGH_BIN overrides
// (CI builds it in an earlier step). Workers inherit process.env set
// here, which is how helpers/bough.ts finds it.
import { execFileSync } from 'child_process';
import * as path from 'path';

const repoRoot = path.resolve(__dirname, '..', '..', '..');

export default function globalSetup(): void {
  if (process.env.BOUGH_BIN) return; // caller built it already
  const bin = execFileSync('go', ['run', './internal/testbin/boughbin'], {
    cwd: repoRoot,
    stdio: ['ignore', 'pipe', 'inherit'],
  })
    .toString()
    .trim();
  process.env.BOUGH_BIN = bin;
}
