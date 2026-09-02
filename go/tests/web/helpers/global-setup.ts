// Build the bough binary ONCE per suite run. Tests exec the built
// binary; BOUGH_BIN overrides (CI builds it in an earlier step).
import { execFileSync } from 'child_process';
import * as path from 'path';

const repoRoot = path.resolve(__dirname, '..', '..', '..');

export default function globalSetup(): void {
  if (process.env.BOUGH_BIN) return; // caller built it already
  execFileSync('go', ['build', '-o', path.join(repoRoot, 'bough'), './cmd/bough'], {
    cwd: repoRoot,
    stdio: 'inherit',
  });
}
